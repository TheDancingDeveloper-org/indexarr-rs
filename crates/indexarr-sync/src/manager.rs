use std::collections::VecDeque;
use std::sync::Arc;

use chrono::Utc;
use sqlx::PgPool;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use indexarr_core::config::Settings;
use indexarr_identity::{BanList, ContributorIdentity};

use crate::delta::DeltaExporter;
use crate::discovery::PeerTable;
use crate::epoch;
use crate::merge;
use crate::{SyncActivity, SyncDashboard};

/// P2P sync manager — orchestrates export, discovery, and gossip loops.
pub struct SyncManager {
    pool: PgPool,
    settings: SyncConfig,
    exporter: DeltaExporter,
    peer_table: Arc<RwLock<PeerTable>>,
    identity: Arc<tokio::sync::RwLock<ContributorIdentity>>,
    ban_list: Arc<tokio::sync::RwLock<BanList>>,
    activity: std::sync::Mutex<VecDeque<SyncActivity>>,
    export_sequence: std::sync::Mutex<i64>,
}

#[derive(Clone)]
pub struct SyncConfig {
    pub export_interval: u64,
    pub import_interval: u64,
    pub discovery_interval: u64,
    pub gossip_fanout: u32,
    pub max_delta_size: u32,
    pub import_categories: Vec<String>,
    pub data_dir: std::path::PathBuf,
    pub sync_peers: Vec<String>,
    pub push_enabled: bool,
    pub verify_tls: bool,
    pub maintainer_pubkey: String,
}

impl SyncConfig {
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            export_interval: settings.sync_export_interval,
            import_interval: settings.sync_import_interval,
            discovery_interval: settings.sync_discovery_interval,
            gossip_fanout: settings.gossip_fanout,
            max_delta_size: settings.sync_max_delta_size,
            import_categories: settings.sync_import_categories.clone(),
            data_dir: settings.data_dir.clone(),
            sync_peers: settings.sync_peers.clone(),
            push_enabled: settings.sync_push_enabled,
            verify_tls: settings.sync_verify_tls,
            maintainer_pubkey: settings.swarm_maintainer_pubkey.clone(),
        }
    }
}

impl SyncManager {
    pub fn new(
        pool: PgPool,
        settings: SyncConfig,
        identity: Arc<tokio::sync::RwLock<ContributorIdentity>>,
        ban_list: Arc<tokio::sync::RwLock<BanList>>,
    ) -> Self {
        let peer_table = PeerTable::new(200, 100.0, 10000.0, 20.0, 10.0, 0.1, 5.0);
        let exporter = DeltaExporter::new(&settings.data_dir, settings.max_delta_size);

        Self {
            pool,
            settings,
            exporter,
            peer_table: Arc::new(RwLock::new(peer_table)),
            identity,
            ban_list,
            activity: std::sync::Mutex::new(VecDeque::with_capacity(100)),
            export_sequence: std::sync::Mutex::new(0),
        }
    }

    /// Clone the peer-table handle so external discovery channels
    /// (e.g. XMPP) can insert peers into the same table.
    pub fn peer_table_handle(&self) -> Arc<RwLock<PeerTable>> {
        self.peer_table.clone()
    }

    pub async fn run(&self, cancel: CancellationToken) {
        tracing::info!("sync manager starting");

        // Load peer table + bootstrap
        {
            let contributor_id = self
                .identity
                .read()
                .await
                .contributor_id()
                .map(str::to_string);
            let mut pt = self.peer_table.write().await;
            pt.set_local_contributor(contributor_id);
            let _ = pt.load(&self.pool).await;
            pt.add_bootstrap_peers(&self.settings.sync_peers);
        }

        self.log_activity("bootstrap", "sync manager started", None);

        tokio::select! {
            _ = self.export_loop(cancel.clone()) => {}
            _ = self.discovery_loop(cancel.clone()) => {}
            _ = self.gossip_loop(cancel.clone()) => {}
            _ = cancel.cancelled() => {}
        }

        // Persist on shutdown
        let pt = self.peer_table.read().await;
        let _ = pt.persist(&self.pool).await;
        tracing::info!("sync manager stopped");
    }

    async fn export_loop(&self, cancel: CancellationToken) {
        loop {
            if cancel.is_cancelled() {
                break;
            }

            let epoch = epoch::get_current_epoch(&self.settings.data_dir);
            // Lock identity briefly, then release before await
            let identity = self.identity.read().await;
            let export_result = self
                .exporter
                .export_delta(&self.pool, &identity, epoch)
                .await;
            drop(identity);

            match export_result {
                Ok(Some((_path, hash, count))) => {
                    if let Ok(mut seq) = self.export_sequence.lock() {
                        *seq += 1;
                    }
                    self.log_activity(
                        "export",
                        &format!("exported {count} records ({hash})"),
                        None,
                    );
                }
                Ok(None) => {}
                Err(e) => {
                    self.log_activity("error", &format!("export failed: {e}"), None);
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(
                self.settings.export_interval,
            ))
            .await;
        }
    }

    async fn discovery_loop(&self, cancel: CancellationToken) {
        loop {
            if cancel.is_cancelled() {
                break;
            }

            // PEX with known peers
            let urls: Vec<String> = {
                let pt = self.peer_table.read().await;
                pt.peers().map(|p| p.url.clone()).collect()
            };

            let client = build_http_client(self.settings.verify_tls);
            for url in &urls {
                if cancel.is_cancelled() {
                    break;
                }
                let peers_url = format!("{url}/api/v1/sync/peers");
                if let Ok(resp) = client
                    .get(&peers_url)
                    .timeout(std::time::Duration::from_secs(10))
                    .send()
                    .await
                    && resp.status().is_success()
                    && let Ok(body) = resp.json::<serde_json::Value>().await
                    && let Some(peers) = body.get("peers").and_then(|v| v.as_array())
                {
                    let mut pt = self.peer_table.write().await;
                    for peer in peers {
                        let pu = peer.get("url").and_then(|v| v.as_str()).unwrap_or("");
                        let pi = peer
                            .get("contributor_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        if !pu.is_empty() && !pi.is_empty() {
                            pt.add_peer(pi, pu, "pex");
                        }
                    }
                }
            }

            {
                let mut pt = self.peer_table.write().await;
                pt.apply_longevity_bonus(self.settings.discovery_interval);
            }
            {
                let pt = self.peer_table.read().await;
                let _ = pt.persist(&self.pool).await;
                self.log_activity(
                    "discovery",
                    &format!("{} peers ({} healthy)", pt.peer_count(), pt.healthy_count()),
                    None,
                );
            }

            tokio::time::sleep(std::time::Duration::from_secs(
                self.settings.discovery_interval,
            ))
            .await;
        }
    }

    async fn gossip_loop(&self, cancel: CancellationToken) {
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;

        loop {
            if cancel.is_cancelled() {
                break;
            }

            let targets = {
                let pt = self.peer_table.read().await;
                pt.get_gossip_targets(self.settings.gossip_fanout as usize)
                    .into_iter()
                    .cloned()
                    .collect::<Vec<_>>()
            };

            if targets.is_empty() {
                tokio::time::sleep(std::time::Duration::from_secs(
                    self.settings.import_interval,
                ))
                .await;
                continue;
            }

            let client = build_http_client(self.settings.verify_tls);

            for peer in &targets {
                if cancel.is_cancelled() {
                    break;
                }
                match self.gossip_with_peer(&client, peer).await {
                    Ok(merged) => {
                        let mut pt = self.peer_table.write().await;
                        pt.update_health(&peer.peer_id, true);
                        if merged > 0 {
                            pt.apply_contribution_bonus(&peer.peer_id, merged);
                        }
                        drop(pt);
                        self.log_activity(
                            "gossip",
                            &format!("merged {merged} from {}", peer.peer_id),
                            Some(&peer.peer_id),
                        );
                    }
                    Err(e) => {
                        let mut pt = self.peer_table.write().await;
                        pt.update_health(&peer.peer_id, false);
                        drop(pt);
                        self.log_activity(
                            "error",
                            &format!("gossip with {} failed: {e}", peer.peer_id),
                            Some(&peer.peer_id),
                        );
                    }
                }
            }

            tokio::time::sleep(std::time::Duration::from_secs(
                self.settings.import_interval,
            ))
            .await;
        }
    }

    async fn gossip_with_peer(
        &self,
        client: &reqwest::Client,
        peer: &crate::discovery::PeerInfo,
    ) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
        let manifest_url = format!("{}/api/v1/sync/manifest", peer.url);
        let resp = client
            .get(&manifest_url)
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(format!("manifest fetch: {}", resp.status()).into());
        }
        let manifest: crate::delta::Manifest = resp.json().await?;

        let our_watermark = {
            let pt = self.peer_table.read().await;
            pt.peers()
                .find(|p| p.peer_id == peer.peer_id)
                .map(|p| p.last_sequence)
                .unwrap_or(0)
        };

        let new_deltas: Vec<_> = manifest
            .deltas
            .iter()
            .filter(|d| d.sequence > our_watermark)
            .collect();
        if new_deltas.is_empty() {
            return Ok(0);
        }

        let mut total_merged = 0u64;
        let mut max_seq = our_watermark;

        for delta_info in &new_deltas {
            let delta_url = format!("{}/api/v1/sync/delta/{}", peer.url, delta_info.content_hash);
            let resp = client
                .get(&delta_url)
                .timeout(std::time::Duration::from_secs(30))
                .send()
                .await?;
            if !resp.status().is_success() {
                continue;
            }

            let data = resp.bytes().await?;
            let temp_path = self
                .settings
                .data_dir
                .join("sync")
                .join(format!("tmp_{}.ndjson.gz", delta_info.content_hash));
            std::fs::write(&temp_path, &data)?;

            let ban_list = self.ban_list.read().await;
            let stats = merge::merge_delta(
                &self.pool,
                &temp_path,
                &self.settings.data_dir,
                &ban_list,
                &self.settings.import_categories,
                false,
            )
            .await?;

            total_merged += stats.inserted + stats.updated;
            max_seq = max_seq.max(delta_info.sequence);
            let _ = std::fs::remove_file(&temp_path);
        }

        if max_seq > our_watermark {
            let mut pt = self.peer_table.write().await;
            pt.update_watermark(&peer.peer_id, max_seq);
        }

        Ok(total_merged)
    }

    pub async fn dashboard(&self) -> SyncDashboard {
        let pt = self.peer_table.read().await;
        let activity = self.activity.lock().unwrap();
        SyncDashboard {
            enabled: true,
            export_sequence: *self.export_sequence.lock().unwrap(),
            peer_count: pt.peer_count(),
            healthy_peers: pt.healthy_count(),
            last_export: None,
            last_gossip: None,
            epoch: epoch::get_current_epoch(&self.settings.data_dir),
            activity: activity.iter().cloned().collect(),
        }
    }

    fn log_activity(&self, event: &str, message: &str, peer_id: Option<&str>) {
        if let Ok(mut activity) = self.activity.lock() {
            if activity.len() >= 100 {
                activity.pop_back();
            }
            activity.push_front(SyncActivity {
                timestamp: Utc::now(),
                event: event.to_string(),
                message: message.to_string(),
                peer_id: peer_id.map(String::from),
            });
        }
    }
}

fn build_http_client(verify_tls: bool) -> reqwest::Client {
    let mut builder = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(10));
    if !verify_tls {
        builder = builder.danger_accept_invalid_certs(true);
    }
    builder.build().unwrap_or_default()
}
