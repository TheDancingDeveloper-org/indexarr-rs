use clap::Parser;
use tokio_util::sync::CancellationToken;

use indexarr_core::config::{DbBackend, Settings};
use indexarr_core::db;
use indexarr_identity::ContributorIdentity;
use indexarr_web::state::AppState;

#[derive(Parser)]
#[command(
    name = "indexarr",
    about = "Decentralized torrent indexing",
    version = "0.1.0"
)]
struct Cli {
    /// Run all workers
    #[arg(long)]
    all: bool,

    /// Comma-separated list of workers to run
    #[arg(long)]
    workers: Option<String>,

    /// HTTP listen address
    #[arg(long)]
    host: Option<String>,

    /// HTTP listen port
    #[arg(long)]
    port: Option<u16>,

    /// Enable debug mode
    #[arg(long)]
    debug: bool,

    /// Database backend (postgresql or sqlite)
    #[arg(long, value_enum)]
    db_backend: Option<DbBackend>,

    /// Database URL
    #[arg(long)]
    db_url: Option<String>,
}

fn crawler_requested(workers: &[String]) -> bool {
    workers
        .iter()
        .any(|worker| worker == "dht_crawler" || worker == "bep51_sampler")
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Load .env if present
    let _ = dotenvy::dotenv();

    let cli = Cli::parse();

    // Load settings from env, then apply CLI overrides
    let mut settings = Settings::from_env();
    if let Some(host) = cli.host {
        settings.host = host;
    }
    if let Some(port) = cli.port {
        settings.port = port;
    }
    if cli.debug {
        settings.debug = true;
    }
    if let Some(backend) = cli.db_backend {
        settings.db_backend = backend;
    }
    if let Some(url) = cli.db_url {
        settings.db_url = url;
    }

    // Set up log capture + tracing
    let log_capture = indexarr_web::log_capture::LogCapture::new(5000);
    let log_layer = indexarr_web::log_capture::LogCaptureLayer::new(log_capture.clone());

    let filter = if settings.debug { "debug" } else { "info" };
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    // Always write logs to data/indexarr.log so they survive a closed console window
    let data_dir = std::path::PathBuf::from(&settings.data_dir);
    let _ = std::fs::create_dir_all(&data_dir);
    let file_layer = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(data_dir.join("indexarr.log"))
        .ok()
        .map(|f| {
            tracing_subscriber::fmt::layer()
                .with_writer(std::sync::Mutex::new(f))
                .with_ansi(false)
        });

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(filter)),
        )
        .with(tracing_subscriber::fmt::layer())
        .with(log_layer)
        .with(file_layer)
        .init();

    // Initialize contributor identity
    let mut identity = ContributorIdentity::new(&settings.data_dir);

    if !settings.contributor_recovery_key.is_empty() {
        match identity.restore_from_recovery_key(&settings.contributor_recovery_key) {
            Ok(()) => {
                identity.acknowledge_onboarding();
                tracing::info!(
                    id = identity.contributor_id().unwrap_or("?"),
                    "restored identity from env"
                );
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to restore identity from env");
                let (is_new, recovery_key) = identity.load_or_generate()?;
                log_identity(&identity, is_new, recovery_key.as_deref());
            }
        }
    } else {
        let (is_new, recovery_key) = identity.load_or_generate()?;
        log_identity(&identity, is_new, recovery_key.as_deref());
    }

    // Determine workers
    let workers: Vec<String> = if cli.all {
        vec![
            "http_server",
            "dht_crawler",
            "resolver",
            "announcer",
            "sync",
            "peer_refresher",
        ]
        .into_iter()
        .map(String::from)
        .collect()
    } else if let Some(ref w) = cli.workers {
        w.split(',').map(|s| s.trim().to_string()).collect()
    } else {
        settings.workers.clone()
    };

    tracing::info!(
        workers = workers.join(", "),
        "Indexarr v0.1.0 — starting workers"
    );

    // Reflect the actually-resolved worker list back into settings so HTTP
    // handlers (e.g. /api/v1/dht/status, /announcer/status) can report which
    // workers are scheduled on this node. Without this they stay on the
    // env-derived value, which is empty for `--all` invocations.
    settings.workers = workers.clone();

    // Initialize database
    let pool = db::init_db(&settings).await?;
    tracing::info!("database initialized");

    // Build shared state
    let host = settings.host.clone();
    let port = settings.port;
    let state = AppState::new(pool, settings, identity, log_capture);

    // Cancellation token for graceful shutdown
    let cancel = CancellationToken::new();

    // Handle SIGINT/SIGTERM
    let cancel_signal = cancel.clone();
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            let ctrl_c = tokio::signal::ctrl_c();
            let mut sigterm =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("failed to listen for SIGTERM");
            tokio::select! {
                _ = ctrl_c => {}
                _ = sigterm.recv() => {}
            }
        }
        #[cfg(not(unix))]
        {
            tokio::signal::ctrl_c().await.ok();
        }
        tracing::info!("shutdown signal received");
        cancel_signal.cancel();
    });

    // Mark app ready once DB is confirmed reachable
    let state_ready = state.clone();
    tokio::spawn(async move {
        // DB is already connected at this point, mark ready
        state_ready.set_ready();
        tracing::info!("all systems ready");
    });

    // Start workers
    let mut handles = Vec::new();

    if workers.iter().any(|w| w == "http_server") {
        let state = state.clone();
        let cancel = cancel.clone();
        let host = host.clone();
        handles.push(tokio::spawn(async move {
            if let Err(e) = indexarr_web::run_server(state, &host, port, cancel).await {
                tracing::error!(error = %e, "HTTP server error");
            }
        }));
    }

    // DHT crawler + resolver + peer refresher + BEP 51 sampler all share
    // the DHT engine / shared state.
    let needs_dht = workers.iter().any(|w| {
        w == "dht_crawler" || w == "resolver" || w == "peer_refresher" || w == "bep51_sampler"
    });
    let _dht_engine = if needs_dht {
        let dht_shared = indexarr_dht::DhtSharedState::new();
        let dht_instances = state.settings.dht_instances;
        let dht_base_port = state.settings.dht_base_port;

        match indexarr_dht::engine::DhtEngine::new(
            dht_instances,
            dht_base_port,
            dht_shared.clone(),
            cancel.clone(),
        )
        .await
        {
            Ok(engine) => {
                let engine = std::sync::Arc::new(engine);

                // Start hash ingest worker
                let ingest_pool = state.pool.clone();
                let ingest_shared = dht_shared.clone();
                let ingest_cancel = cancel.clone();
                handles.push(tokio::spawn(async move {
                    indexarr_dht::ingest::run_hash_ingest(
                        ingest_pool,
                        ingest_shared,
                        ingest_cancel,
                    )
                    .await;
                }));

                // Start resolver
                if workers.iter().any(|w| w == "resolver") {
                    // Shared peer_id between BEP 9 fetches and tracker announces
                    // — both want a stable identity per resolver instance.
                    let resolver_peer_id = indexarr_dht::random_peer_id();
                    // UDP bind failure here is unrecoverable — we'd be running
                    // without any tracker discovery, so just fail loudly at startup.
                    let tracker_discovery = std::sync::Arc::new(
                        indexarr_dht::tracker_announce::TrackerDiscovery::new(
                            cancel.clone(),
                            resolver_peer_id,
                            state.settings.dht_base_port,
                        )
                        .await
                        .expect(
                            "TrackerDiscovery: failed to bind UDP socket for tracker announces",
                        ),
                    );

                    let resolver = indexarr_dht::resolver::MetadataResolver::new(
                        state.pool.clone(),
                        dht_shared.clone(),
                        engine.clone(),
                        state.settings.resolve_workers as usize,
                        state.settings.resolve_timeout as u64,
                        state.settings.save_files_threshold,
                        cancel.clone(),
                        resolver_peer_id,
                        tracker_discovery,
                    );
                    handles.push(tokio::spawn(async move {
                        resolver.run().await;
                    }));
                }

                // Peer-count refresher — queries DHT for peer counts on
                // stale or trackerless torrents.
                if workers.iter().any(|w| w == "peer_refresher") {
                    let refresh_pool = state.pool.clone();
                    let refresh_engine = engine.clone();
                    let refresh_cancel = cancel.clone();
                    let interval = state.settings.peer_refresh_interval;
                    let batch = state.settings.peer_refresh_batch as usize;
                    handles.push(tokio::spawn(async move {
                        indexarr_dht::peer_refresher::run_peer_refresher(
                            refresh_pool,
                            refresh_engine,
                            refresh_cancel,
                            batch,
                            interval,
                        )
                        .await;
                    }));
                }

                // The crawler discovers real hashes with BEP 51. Keep
                // `bep51_sampler` as a backwards-compatible worker alias.
                // `get_peers` is only valid for peer lookup of a known hash;
                // random get_peers targets must never be stored as torrents.
                let crawl_requested = crawler_requested(&workers);
                if crawl_requested && state.settings.dht_enable_bep51 {
                    let bep51_shared = dht_shared.clone();
                    let bep51_engine = engine.clone();
                    let bep51_cancel = cancel.clone();
                    handles.push(tokio::spawn(async move {
                        indexarr_dht::bep51_sampler::run_bep51_sampler(
                            bep51_shared,
                            bep51_engine,
                            bep51_cancel,
                        )
                        .await;
                    }));
                } else if crawl_requested {
                    tracing::warn!(
                        "DHT crawler disabled because INDEXARR_DHT_ENABLE_BEP51 is false"
                    );
                }

                Some(engine)
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to start DHT engine");
                None
            }
        }
    } else {
        None
    };

    // Announcer worker
    if workers.iter().any(|w| w == "announcer") && state.settings.announcer_enabled {
        let pool = state.pool.clone();
        let config = indexarr_announcer::AnnouncerConfig::from_settings(&state.settings);
        let cancel = cancel.clone();
        handles.push(tokio::spawn(async move {
            let mut announcer = indexarr_announcer::TorrentAnnouncer::new(pool, config);
            announcer.run(cancel).await;
        }));
    }

    // Sync worker
    if workers.iter().any(|w| w == "sync") && state.settings.sync_enabled {
        let sync_config = indexarr_sync::manager::SyncConfig::from_settings(&state.settings);
        let identity = std::sync::Arc::new(tokio::sync::RwLock::new(
            indexarr_identity::ContributorIdentity::new(&state.settings.data_dir),
        ));
        {
            let mut id = identity.write().await;
            let _ = id.load_or_generate();
        }

        let ban_list =
            std::sync::Arc::new(tokio::sync::RwLock::new(indexarr_identity::BanList::new(
                &state.settings.swarm_maintainer_pubkey,
                &state.settings.data_dir,
            )));
        {
            ban_list.write().await.load();
        }

        let manager = indexarr_sync::manager::SyncManager::new(
            state.pool.clone(),
            sync_config,
            identity.clone(),
            ban_list,
        );

        // XMPP peer-discovery channel — shares peer_table with SyncManager
        // so nicks observed in the MUC land alongside HTTP-PEX peers.
        if state.settings.xmpp_enabled {
            let peer_table = manager.peer_table_handle();
            let xmpp_settings = state.settings.clone();
            let xmpp_identity = identity.clone();
            let xmpp_cancel = cancel.clone();
            handles.push(tokio::spawn(async move {
                indexarr_xmpp::XmppChannel::new(xmpp_settings, xmpp_identity, peer_table)
                    .run(xmpp_cancel)
                    .await;
            }));
        }

        let cancel = cancel.clone();
        handles.push(tokio::spawn(async move {
            manager.run(cancel).await;
        }));
    }

    for w in &workers {
        match w.as_str() {
            "http_server" | "dht_crawler" | "resolver" | "announcer" | "sync"
            | "peer_refresher" | "bep51_sampler" => {}
            other => tracing::warn!(worker = other, "unknown worker"),
        }
    }

    if handles.is_empty() {
        tracing::error!("no workers to run — use --all or --workers <list>");
        return Ok(());
    }

    // Wait for all workers
    cancel.cancelled().await;
    tracing::info!("shutting down...");

    for handle in handles {
        let _ = handle.await;
    }

    tracing::info!("shutdown complete");
    Ok(())
}

fn log_identity(identity: &ContributorIdentity, is_new: bool, recovery_key: Option<&str>) {
    if is_new {
        tracing::info!(
            id = identity.contributor_id().unwrap_or("?"),
            "new contributor identity generated"
        );
        if let Some(key) = recovery_key {
            tracing::info!(recovery_key = key, "save your recovery key!");
        }
    } else {
        tracing::info!(
            id = identity.contributor_id().unwrap_or("?"),
            "loaded contributor identity"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::crawler_requested;

    #[test]
    fn dht_crawler_uses_the_bep51_discovery_path() {
        assert!(crawler_requested(&["dht_crawler".to_string()]));
    }

    #[test]
    fn legacy_bep51_worker_name_remains_supported() {
        assert!(crawler_requested(&["bep51_sampler".to_string()]));
    }
}
