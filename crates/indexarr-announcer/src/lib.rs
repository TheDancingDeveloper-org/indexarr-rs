use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use librtbit_core::hash_id::Id20;
use sqlx::{PgPool, Row};
use tokio::net::lookup_host;
use tokio_util::sync::CancellationToken;
use tracker_comms::{AnnounceFields, UdpTrackerClient};
use url::Url;

use indexarr_core::config::Settings;

/// Tracked torrent in the announcer pool.
struct TrackedHandle {
    info_hash: String,
    _name: String,
    trackers: Vec<String>,
    added_at: Instant,
    best_seeds: i32,
    best_peers: i32,
    settled: bool,
    _announce_miss: i32,
}

/// Rolling pool announcer — validates seed/peer counts for resolved torrents.
///
/// Uses HTTP scrape + UDP announce (BEP 15) to get seed/peer counts.
/// Implements 3-strike rule: after 3 consecutive zero-activity scrapes,
/// marks the torrent as no_peers.
pub struct TorrentAnnouncer {
    pool: PgPool,
    settings: AnnouncerConfig,
    tracked: HashMap<String, TrackedHandle>,
    announced_count: u64,
    start_time: Instant,
    /// Stable peer_id we advertise to UDP trackers. Generated at construction.
    peer_id: Id20,
    /// Lazily initialized in `run()` because UdpTrackerClient::new is async.
    udp_client: Option<UdpTrackerClient>,
}

#[derive(Clone)]
pub struct AnnouncerConfig {
    pub pool_size: u32,
    pub poll_interval: u64,
    pub settle_time: u64,
    pub rotate_interval: u64,
    pub default_trackers: Vec<String>,
    /// Port we advertise to trackers (must be non-zero — most trackers reject 0).
    /// No actual listener required: indexers don't accept inbound peers.
    pub announce_port: u16,
}

impl AnnouncerConfig {
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            pool_size: settings.announcer_pool_size,
            poll_interval: settings.announcer_poll_interval,
            settle_time: settings.announcer_settle_time,
            rotate_interval: settings.announcer_rotate_interval,
            default_trackers: settings.default_trackers.clone(),
            announce_port: settings.dht_base_port,
        }
    }
}

impl TorrentAnnouncer {
    pub fn new(pool: PgPool, config: AnnouncerConfig) -> Self {
        let mut bytes = [0u8; 20];
        bytes[..8].copy_from_slice(b"-IDXR01-");
        rand::fill(&mut bytes[8..]);
        Self {
            pool,
            settings: config,
            tracked: HashMap::new(),
            announced_count: 0,
            start_time: Instant::now(),
            peer_id: Id20::new(bytes),
            udp_client: None,
        }
    }

    /// Main announcer loop.
    pub async fn run(&mut self, cancel: CancellationToken) {
        tracing::info!(pool_size = self.settings.pool_size, "announcer started");

        // Initialize the UDP tracker client now that we're in an async context.
        // Failure to bind the UDP socket is non-fatal — fall back to HTTP-only.
        match UdpTrackerClient::new(cancel.clone(), None).await {
            Ok(c) => {
                tracing::info!("announcer: UDP tracker client ready");
                self.udp_client = Some(c);
            }
            Err(e) => {
                tracing::warn!(error = %e, "announcer: UDP tracker client init failed, HTTP-only");
            }
        }

        loop {
            if cancel.is_cancelled() {
                break;
            }

            // 1. Backfill pool with candidates from DB
            if let Err(e) = self.load_candidates().await {
                tracing::error!(error = %e, "failed to load candidates");
            }

            // 2. Poll and harvest settled torrents
            let harvested = self.poll_and_harvest().await;

            // 3. Persist results to DB
            if !harvested.is_empty()
                && let Err(e) = self.persist_results(&harvested).await
            {
                tracing::error!(error = %e, "failed to persist announcer results");
            }

            // 4. Remove harvested from pool
            for (hash, _) in &harvested {
                self.tracked.remove(hash);
            }

            // Stats
            if self.announced_count.is_multiple_of(100) && self.announced_count > 0 {
                tracing::info!(
                    announced = self.announced_count,
                    pool = self.tracked.len(),
                    uptime_secs = self.start_time.elapsed().as_secs(),
                    "announcer stats"
                );
            }

            tokio::time::sleep(std::time::Duration::from_secs(self.settings.poll_interval)).await;
        }

        tracing::info!(total_announced = self.announced_count, "announcer stopped");
    }

    /// Fill pool slots from DB with unannounced/stale torrents.
    async fn load_candidates(&mut self) -> Result<(), sqlx::Error> {
        let free_slots = self.settings.pool_size as usize - self.tracked.len();
        if free_slots == 0 {
            return Ok(());
        }

        let exclude: Vec<&str> = self.tracked.keys().map(|s| s.as_str()).collect();

        // Query candidates: resolved, not no_peers, not already in pool
        let rows = sqlx::query(
            "SELECT info_hash, name, trackers FROM torrents \
             WHERE name IS NOT NULL AND no_peers IS NOT TRUE \
               AND info_hash != ALL($1) \
             ORDER BY announced_at ASC NULLS FIRST \
             LIMIT $2",
        )
        .bind(&exclude)
        .bind(free_slots as i64)
        .fetch_all(&self.pool)
        .await?;

        for row in &rows {
            let hash: String = row.get("info_hash");
            let name: String = row.get::<Option<String>, _>("name").unwrap_or_default();
            let trackers_json: Option<serde_json::Value> = row.get("trackers");
            let trackers = parse_trackers(trackers_json, &self.settings.default_trackers);

            self.tracked.insert(
                hash.clone(),
                TrackedHandle {
                    info_hash: hash,
                    _name: name,
                    trackers,
                    added_at: Instant::now(),
                    best_seeds: 0,
                    best_peers: 0,
                    settled: false,
                    _announce_miss: 0,
                },
            );
        }

        Ok(())
    }

    /// Poll tracker scrape for all tracked torrents and harvest settled ones.
    async fn poll_and_harvest(&mut self) -> Vec<(String, HarvestResult)> {
        let mut harvested = Vec::new();
        let now = Instant::now();

        let hashes: Vec<String> = self.tracked.keys().cloned().collect();
        for hash in hashes {
            let handle = match self.tracked.get_mut(&hash) {
                Some(h) => h,
                None => continue,
            };

            let age = now.duration_since(handle.added_at).as_secs();

            // Mark settled
            if age >= self.settings.settle_time && !handle.settled {
                handle.settled = true;
            }

            // Scrape tracker for seed/peer counts
            let (seeds, peers) = scrape_trackers(
                &handle.info_hash,
                &handle.trackers,
                self.udp_client.as_ref(),
                self.peer_id,
                self.settings.announce_port,
            )
            .await;
            handle.best_seeds = handle.best_seeds.max(seeds);
            handle.best_peers = handle.best_peers.max(peers);

            // Harvest if past rotate interval
            if age >= self.settings.rotate_interval {
                harvested.push((
                    hash.clone(),
                    HarvestResult {
                        seeds: handle.best_seeds,
                        peers: handle.best_peers,
                    },
                ));
            }
        }

        harvested
    }

    /// Persist harvest results to DB.
    async fn persist_results(
        &mut self,
        results: &[(String, HarvestResult)],
    ) -> Result<(), sqlx::Error> {
        for (hash, result) in results {
            let has_activity = result.seeds > 0 || result.peers > 0;

            if has_activity {
                // Update with new counts
                sqlx::query(
                    "UPDATE torrents SET \
                       seed_count = GREATEST(seed_count, $2), \
                       peer_count = GREATEST(peer_count, $3), \
                       announced_at = NOW(), \
                       announce_miss = 0 \
                     WHERE info_hash = $1",
                )
                .bind(hash)
                .bind(result.seeds)
                .bind(result.peers)
                .execute(&self.pool)
                .await?;
            } else {
                // 3-strike rule
                let miss: Option<i32> = sqlx::query_scalar(
                    "UPDATE torrents SET announce_miss = announce_miss + 1, announced_at = NOW() \
                     WHERE info_hash = $1 RETURNING announce_miss",
                )
                .bind(hash)
                .fetch_optional(&self.pool)
                .await?;

                if let Some(miss_count) = miss
                    && miss_count >= 3
                {
                    sqlx::query("UPDATE torrents SET no_peers = TRUE WHERE info_hash = $1")
                        .bind(hash)
                        .execute(&self.pool)
                        .await?;
                    tracing::debug!(hash = %hash, misses = miss_count, "marked no_peers (3-strike)");
                }
            }

            self.announced_count += 1;
        }
        Ok(())
    }
}

struct HarvestResult {
    seeds: i32,
    peers: i32,
}

/// Parse tracker URLs from JSON or use defaults.
fn parse_trackers(json: Option<serde_json::Value>, defaults: &[String]) -> Vec<String> {
    if let Some(serde_json::Value::Array(arr)) = json {
        let trackers: Vec<String> = arr
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
        if !trackers.is_empty() {
            return trackers;
        }
    }
    defaults.to_vec()
}

/// Scrape tracker(s) for seed/peer counts of an info_hash.
///
/// Iterates over configured trackers, trying both HTTP scrape (BEP 48) and
/// UDP announce (BEP 15) until one returns useful counts. Returns the
/// best-observed counts across the tracker list, or (0, 0) if none responded.
///
/// HTTP path: standard scrape — converts `/announce` URL to `/scrape` and
/// parses the bencode response.
///
/// UDP path: connect → announce (action=1) → take seeders/leechers from the
/// AnnounceResponse. We use announce rather than scrape because most UDP
/// trackers respond more reliably to announce, and the count fields are
/// equivalent for our purposes.
async fn scrape_trackers(
    info_hash: &str,
    trackers: &[String],
    udp_client: Option<&UdpTrackerClient>,
    peer_id: Id20,
    announce_port: u16,
) -> (i32, i32) {
    let Ok(hash_bytes) = hex::decode(info_hash) else {
        return (0, 0);
    };
    if hash_bytes.len() != 20 {
        return (0, 0);
    }
    let info_hash_id = Id20::new(hash_bytes.clone().try_into().unwrap_or([0u8; 20]));

    let http_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap_or_default();

    let mut best = (0i32, 0i32);
    for tracker_url in trackers {
        let parsed = match Url::parse(tracker_url) {
            Ok(u) => u,
            Err(_) => continue,
        };
        let result = match parsed.scheme() {
            "http" | "https" => scrape_http(&http_client, tracker_url, &hash_bytes).await,
            "udp" => match udp_client {
                Some(c) => scrape_udp(c, &parsed, info_hash_id, peer_id, announce_port).await,
                None => None,
            },
            _ => None,
        };
        if let Some((s, p)) = result {
            if s > best.0 {
                best.0 = s;
            }
            if p > best.1 {
                best.1 = p;
            }
            // Early exit if we got concrete activity from one tracker — we don't
            // need to query every tracker for every torrent.
            if s > 0 || p > 0 {
                return best;
            }
        }
    }

    best
}

/// HTTP scrape (BEP 48). Returns (seeds, leechers) or None.
async fn scrape_http(
    client: &reqwest::Client,
    tracker_url: &str,
    hash_bytes: &[u8],
) -> Option<(i32, i32)> {
    let scrape_url = tracker_url.replace("/announce", "/scrape");
    let url = format!("{scrape_url}?info_hash={}", url_encode_hash(hash_bytes));
    match client.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => {
            let body = resp.bytes().await.ok()?;
            parse_scrape_response(&body, hash_bytes)
        }
        _ => None,
    }
}

/// UDP announce (BEP 15) — returns (seeders, leechers) or None on failure.
async fn scrape_udp(
    client: &UdpTrackerClient,
    tracker_url: &Url,
    info_hash: Id20,
    peer_id: Id20,
    announce_port: u16,
) -> Option<(i32, i32)> {
    // Resolve the tracker hostname to a SocketAddr — UDP tracker URLs are
    // host:port (no path); resolve via tokio's DNS.
    let host = tracker_url.host_str()?;
    let port = tracker_url.port()?;
    let addr_str = format!("{host}:{port}");
    let addr: SocketAddr = lookup_host(addr_str).await.ok()?.next()?;

    let fields = AnnounceFields {
        info_hash,
        peer_id,
        downloaded: 0,
        left: 0, // we have everything
        uploaded: 0,
        event: 0, // none
        key: rand::random(),
        port: announce_port,
    };

    // BEP 15 announces should normally complete in ~1s; cap at 5s for safety.
    match tokio::time::timeout(Duration::from_secs(5), client.announce(addr, fields)).await {
        Ok(Ok(resp)) => Some((resp.seeders as i32, resp.leechers as i32)),
        Ok(Err(e)) => {
            tracing::trace!(tracker = %tracker_url, error = %e, "UDP announce failed");
            None
        }
        Err(_) => {
            tracing::trace!(tracker = %tracker_url, "UDP announce timed out");
            None
        }
    }
}

/// URL-encode a 20-byte info_hash for tracker scrape.
fn url_encode_hash(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("%{b:02x}")).collect()
}

/// Parse a bencoded scrape response for seed/peer counts.
fn parse_scrape_response(data: &[u8], info_hash: &[u8]) -> Option<(i32, i32)> {
    // Find the info_hash in the raw bytes
    let hash_pos = data.windows(20).position(|w| w == info_hash)?;
    let after = &data[hash_pos + 20..];

    // Extract integers from the bencode after the hash
    let seeds = extract_bencode_int_bytes(after, b"8:complete")?;
    let peers = extract_bencode_int_bytes(after, b"10:incomplete").unwrap_or(0);

    Some((seeds, peers))
}

fn extract_bencode_int_bytes(data: &[u8], key: &[u8]) -> Option<i32> {
    let pos = data.windows(key.len()).position(|w| w == key)?;
    let after_key = &data[pos + key.len()..];
    // Expect 'i' <digits> 'e'
    if after_key.first() != Some(&b'i') {
        return None;
    }
    let end = after_key.iter().position(|&b| b == b'e')?;
    std::str::from_utf8(&after_key[1..end]).ok()?.parse().ok()
}
