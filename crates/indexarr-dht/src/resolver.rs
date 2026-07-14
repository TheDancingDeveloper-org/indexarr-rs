use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bencode::{ByteBufOwned, from_bytes};
use indexarr_resolver_v2::{
    DEFAULT_MAX_CONCURRENT_PEERS, FetchConfig, MAX_METADATA_SIZE, fetch_from_peers,
};
use librtbit_core::hash_id::Id20;
use librtbit_core::torrent_metainfo::TorrentMetaV1Info;
use sqlx::{PgPool, Row};
use tokio_util::sync::CancellationToken;

use crate::DhtSharedState;
use crate::engine::DhtEngine;
use crate::tracker_announce::SharedTrackerDiscovery;

/// Metadata resolver — fetches torrent metadata via BEP 9 and runs content pipeline.
pub struct MetadataResolver {
    pool: PgPool,
    shared: Arc<DhtSharedState>,
    engine: Arc<DhtEngine>,
    workers: usize,
    timeout_secs: u64,
    save_files_threshold: u32,
    cancel: CancellationToken,
    /// Stable peer_id shared with `TrackerDiscovery` so trackers and peers see
    /// the same identity from this resolver instance.
    peer_id: Id20,
    /// Tracker-driven peer discovery (HTTP + UDP popular public trackers).
    tracker_discovery: SharedTrackerDiscovery,
}

impl MetadataResolver {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pool: PgPool,
        shared: Arc<DhtSharedState>,
        engine: Arc<DhtEngine>,
        workers: usize,
        timeout_secs: u64,
        save_files_threshold: u32,
        cancel: CancellationToken,
        peer_id: Id20,
        tracker_discovery: SharedTrackerDiscovery,
    ) -> Self {
        Self {
            pool,
            shared,
            engine,
            workers,
            timeout_secs,
            save_files_threshold,
            cancel,
            peer_id,
            tracker_discovery,
        }
    }

    /// Main resolver loop.
    pub async fn run(&self) {
        tracing::info!(workers = self.workers, "metadata resolver started");

        // Wait for DHT routing tables to populate
        loop {
            if self.cancel.is_cancelled() {
                return;
            }
            let stats = self.engine.stats();
            if stats.total_routing_nodes >= 20 {
                tracing::info!(
                    nodes = stats.total_routing_nodes,
                    "routing table ready, starting resolver"
                );
                break;
            }
            tracing::debug!(
                nodes = stats.total_routing_nodes,
                "waiting for routing table..."
            );
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }

        let mut last_stats = Instant::now();
        let _resolved_count = 0u64;
        let _failed_count = 0u64;
        let semaphore = Arc::new(tokio::sync::Semaphore::new(self.workers));

        loop {
            if self.cancel.is_cancelled() {
                break;
            }

            // Get unresolved hashes to work on
            let batch = self.get_unresolved_batch(self.workers * 2).await;

            if batch.is_empty() {
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                continue;
            }

            // Discover peers for the batch via DHT
            let hash_strings: Vec<String> = batch.to_vec();
            let discovered = self.engine.discover_peers(&hash_strings).await;

            // Also check cached peers
            for hash in &hash_strings {
                if !discovered.contains_key(hash) {
                    let cached = self.shared.get_peers(hash);
                    if !cached.is_empty() {
                        let addrs: Vec<SocketAddr> = cached
                            .iter()
                            .filter_map(|(ip, port)| format!("{ip}:{port}").parse().ok())
                            .collect();
                        if !addrs.is_empty() {
                            discovered.insert(hash.clone(), addrs);
                        }
                    }
                }
            }

            // Resolve EVERY hash in the batch — even those with no DHT/cache
            // peers. The spawn task announces against the tracker list and
            // can rescue hashes that DHT couldn't find peers for.
            for hash in &hash_strings {
                if self.cancel.is_cancelled() {
                    break;
                }
                let hash = hash.clone();
                let peers: Vec<SocketAddr> = discovered
                    .get(&hash)
                    .map(|e| e.value().clone())
                    .unwrap_or_default();

                let permit = match semaphore.clone().acquire_owned().await {
                    Ok(p) => p,
                    Err(_) => break,
                };

                let pool = self.pool.clone();
                let timeout = self.timeout_secs;
                let threshold = self.save_files_threshold;
                let cancel = self.cancel.clone();
                let peer_id = self.peer_id;
                let tracker_discovery = self.tracker_discovery.clone();
                let shared = self.shared.clone();

                tokio::spawn(async move {
                    let _permit = permit;
                    if cancel.is_cancelled() {
                        return;
                    }

                    // Reserve this attempt before doing network I/O. The
                    // exponential retry time prevents the main loop from
                    // scheduling the same failing hash in a rapid burst.
                    let _ = sqlx::query(
                        "UPDATE torrents \
                         SET resolve_attempts = resolve_attempts + 1, \
                             retry_after = NOW() + (LEAST(86400::double precision, \
                                 30 * power(2, LEAST(resolve_attempts, 12))) * INTERVAL '1 second') \
                         WHERE info_hash = $1",
                    )
                        .bind(&hash)
                        .execute(&pool)
                        .await;

                    // Augment the DHT/cache-discovered peer set with addresses
                    // harvested from announces against popular public trackers.
                    // Run in parallel with the (already-completed) DHT discovery
                    // — bounded to 5s so a slow tracker doesn't stall the fetch.
                    let mut peers = peers;
                    if let Ok(id20) = Id20::from_str(&hash) {
                        let tracker_peers = tracker_discovery
                            .discover_peers(id20, std::time::Duration::from_secs(5))
                            .await;
                        if !tracker_peers.is_empty() {
                            tracing::debug!(
                                hash = %hash,
                                dht = peers.len(),
                                trackers = tracker_peers.len(),
                                "tracker announce complete"
                            );
                            // Dedupe via HashSet, then collect.
                            let mut seen: std::collections::HashSet<SocketAddr> =
                                peers.iter().copied().collect();
                            for addr in tracker_peers {
                                if seen.insert(addr) {
                                    peers.push(addr);
                                }
                            }
                        }
                    }

                    match fetch_metadata(&hash, &peers, timeout, peer_id).await {
                        Ok((meta, harvested_peers, harvested_trackers)) => {
                            // Free peers from the winning peer's ut_pex messages —
                            // fold them into shared.peer_cache for the next batch.
                            let cached = if !harvested_peers.is_empty() {
                                shared.cache_peers(&hash, harvested_peers.iter().copied())
                            } else {
                                0
                            };
                            tracing::info!(
                                hash = %hash,
                                peers = peers.len(),
                                size = meta.size,
                                files = meta.files.len(),
                                pex = harvested_peers.len(),
                                pex_cached = cached,
                                lt_tex = harvested_trackers.len(),
                                "BEP 9 fetch ok"
                            );
                            if let Err(e) = process_resolved(&pool, &hash, &meta, threshold).await {
                                tracing::warn!(hash = %hash, error = %e, "failed to store metadata");
                            }
                            if !harvested_trackers.is_empty() {
                                let _ = merge_trackers(&pool, &hash, &harvested_trackers)
                                    .await
                                    .map_err(|e| tracing::warn!(hash = %hash, error = %e, "failed to merge lt_tex trackers"));
                            }
                        }
                        Err(e) => {
                            tracing::info!(
                                hash = %hash,
                                peers = peers.len(),
                                error = %e,
                                "BEP 9 fetch failed"
                            );
                        }
                    }
                });
            }

            // Stats
            if last_stats.elapsed().as_secs() >= 30 {
                let total: i64 = sqlx::query_scalar(
                    "SELECT COUNT(*) FROM torrents WHERE resolved_at IS NOT NULL",
                )
                .fetch_one(&self.pool)
                .await
                .unwrap_or(0);
                tracing::info!(total_resolved = total, "resolver stats");
                last_stats = Instant::now();
            }

            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }

        tracing::info!("metadata resolver stopped");
    }

    /// Get a batch of unresolved info_hashes from the DB.
    async fn get_unresolved_batch(&self, limit: usize) -> Vec<String> {
        // First check cached peers for fast path
        let cached_hashes: Vec<String> = self
            .shared
            .peer_cache
            .iter()
            .take(limit * 2)
            .map(|e| e.key().clone())
            .collect();

        if !cached_hashes.is_empty() {
            // Filter to unresolved only
            let rows = sqlx::query(
                "SELECT info_hash FROM torrents \
                 WHERE info_hash = ANY($1) \
                   AND resolved_at IS NULL \
                   AND (retry_after IS NULL OR retry_after <= NOW()) \
                   AND source != 'uploaded' \
                 ORDER BY priority DESC, observations DESC \
                 LIMIT $2",
            )
            .bind(&cached_hashes)
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await;

            if let Ok(rows) = rows {
                let mut result: Vec<String> = rows.iter().map(|r| r.get("info_hash")).collect();
                if result.len() >= limit {
                    return result;
                }

                // Fill remaining from DB
                let remaining = limit - result.len();
                if let Ok(more) = sqlx::query(
                    "SELECT info_hash FROM torrents \
                     WHERE resolved_at IS NULL \
                       AND (retry_after IS NULL OR retry_after <= NOW()) \
                       AND source != 'uploaded' \
                     ORDER BY priority DESC, \
                              CASE WHEN source = 'announce' THEN 0 ELSE 1 END, \
                              observations DESC, \
                              discovered_at DESC \
                     LIMIT $1",
                )
                .bind(remaining as i64)
                .fetch_all(&self.pool)
                .await
                {
                    for r in more {
                        let h: String = r.get("info_hash");
                        if !result.contains(&h) {
                            result.push(h);
                        }
                    }
                }
                return result;
            }
        }

        // Fallback: just query DB
        sqlx::query(
            "SELECT info_hash FROM torrents \
             WHERE resolved_at IS NULL \
               AND (retry_after IS NULL OR retry_after <= NOW()) \
               AND source != 'uploaded' \
             ORDER BY priority DESC, \
                      CASE WHEN source = 'announce' THEN 0 ELSE 1 END, \
                      observations DESC, \
                      discovered_at DESC \
             LIMIT $1",
        )
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default()
        .iter()
        .map(|r| r.get("info_hash"))
        .collect()
    }
}

/// Resolved metadata from a peer.
#[derive(Debug)]
struct ResolvedMeta {
    name: String,
    size: i64,
    files: Vec<FileEntry>,
    is_private: bool,
    piece_length: Option<i32>,
    piece_count: Option<i32>,
    seed_count: i32,
    peer_count: i32,
}

#[derive(Debug)]
struct FileEntry {
    path: String,
    size: i64,
}

/// Fetch metadata for an info_hash from a list of candidate peers via BEP 9.
///
/// Returns `(meta, harvested_peers, harvested_trackers)`.
async fn fetch_metadata(
    info_hash: &str,
    peers: &[SocketAddr],
    timeout_secs: u64,
    peer_id: Id20,
) -> Result<(ResolvedMeta, Vec<SocketAddr>, Vec<String>), String> {
    if peers.is_empty() {
        return Err(format!("no peers available for {info_hash}"));
    }

    let id = Id20::from_str(info_hash).map_err(|e| format!("invalid info_hash: {e}"))?;
    let cfg = FetchConfig {
        timeout: Duration::from_secs(timeout_secs.max(1)),
        max_metadata_size: MAX_METADATA_SIZE,
    };

    let fetched = fetch_from_peers(id, peers, peer_id, cfg, DEFAULT_MAX_CONCURRENT_PEERS)
        .await
        .map_err(|e| format!("all {} peers failed (last: {e})", peers.len()))?;

    let meta = parse_info_dict(&fetched.bytes)
        .map_err(|e| format!("info dict parse failed after BEP 9 fetch: {e}"))?;
    Ok((meta, fetched.harvested_peers, fetched.harvested_trackers))
}

/// Merge tracker URLs harvested via BEP 28 (lt_tex) into the DB for
/// `info_hash`. De-duplication is handled in SQL — we append only URLs not
/// already present in the existing JSONB array.
async fn merge_trackers(
    pool: &PgPool,
    info_hash: &str,
    trackers: &[String],
) -> Result<(), sqlx::Error> {
    let json = serde_json::to_value(trackers).unwrap_or(serde_json::Value::Array(vec![]));
    sqlx::query(
        "UPDATE torrents \
         SET trackers = ( \
           SELECT jsonb_agg(DISTINCT t) \
           FROM jsonb_array_elements( \
             COALESCE(trackers, '[]'::jsonb) || $2::jsonb \
           ) AS t \
         ) \
         WHERE info_hash = $1",
    )
    .bind(info_hash)
    .bind(&json)
    .execute(pool)
    .await?;
    Ok(())
}

/// Parse a bencoded `info` dict into the resolver's `ResolvedMeta` shape.
fn parse_info_dict(bytes: &[u8]) -> Result<ResolvedMeta, String> {
    let info: TorrentMetaV1Info<ByteBufOwned> =
        from_bytes(bytes).map_err(|e| format!("bencode decode: {e}"))?;

    let name = info
        .name
        .as_ref()
        .map(|b| String::from_utf8_lossy(b.as_ref()).into_owned())
        .unwrap_or_default();

    let mut files: Vec<FileEntry> = Vec::new();
    let mut total_size: i64 = 0;

    if let Some(file_list) = &info.files {
        // Multi-file torrent.
        for f in file_list {
            let path = f
                .path
                .iter()
                .map(|seg| String::from_utf8_lossy(seg.as_ref()).into_owned())
                .collect::<Vec<_>>()
                .join("/");
            let size = f.length as i64;
            total_size = total_size.saturating_add(size);
            files.push(FileEntry { path, size });
        }
    } else if let Some(length) = info.length {
        // Single-file torrent.
        total_size = length as i64;
        files.push(FileEntry {
            path: name.clone(),
            size: total_size,
        });
    }

    let piece_count = info.pieces.as_ref().map(|p| (p.as_ref().len() / 20) as i32);
    let piece_length = i32::try_from(info.piece_length).ok();

    Ok(ResolvedMeta {
        name,
        size: total_size,
        files,
        is_private: info.private,
        piece_length,
        piece_count,
        // BEP 9 doesn't carry seed/peer counts — left at 0 here. Caller's
        // SQL update uses GREATEST(seed_count, $5) so existing values from the
        // tracker scraper are preserved.
        seed_count: 0,
        peer_count: 0,
    })
}

/// Process resolved metadata — store in DB and run content pipeline.
async fn process_resolved(
    pool: &PgPool,
    info_hash: &str,
    meta: &ResolvedMeta,
    save_files_threshold: u32,
) -> Result<(), sqlx::Error> {
    // Parse and classify
    let parsed = indexarr_parser::parse(&meta.name);
    let file_infos: Vec<indexarr_classifier::FileInfo> = meta
        .files
        .iter()
        .map(|f| indexarr_classifier::FileInfo {
            path: f.path.clone(),
            size: f.size,
            extension: f.path.rsplit('.').next().map(|s| s.to_string()),
        })
        .collect();

    let classification = indexarr_classifier::classify(&parsed, &file_infos, &meta.name);
    let quality_score = indexarr_classifier::compute_quality_score(&parsed);

    // Update torrent record
    sqlx::query(
        "UPDATE torrents SET \
           name = $2, size = $3, resolved_at = NOW(), private = $4, \
           seed_count = GREATEST(seed_count, $5), peer_count = GREATEST(peer_count, $6), \
           no_peers = FALSE, priority = FALSE, \
           piece_length = $7, piece_count = $8 \
         WHERE info_hash = $1",
    )
    .bind(info_hash)
    .bind(&meta.name)
    .bind(meta.size)
    .bind(meta.is_private)
    .bind(meta.seed_count)
    .bind(meta.peer_count)
    .bind(meta.piece_length)
    .bind(meta.piece_count)
    .execute(pool)
    .await?;

    // Store files (if under threshold)
    if meta.files.len() <= save_files_threshold as usize {
        for file in &meta.files {
            let ext = file.path.rsplit('.').next().map(|e| e.to_lowercase());
            sqlx::query(
                "INSERT INTO torrent_files (info_hash, path, size, extension) VALUES ($1, $2, $3, $4)"
            )
            .bind(info_hash)
            .bind(&file.path)
            .bind(file.size)
            .bind(&ext)
            .execute(pool)
            .await?;
        }
    }

    // Store content classification
    sqlx::query(
        "INSERT INTO torrent_content (info_hash, content_type, title, year, season, episode, \
         \"group\", language, resolution, codec, video_source, modifier, is_3d, hdr, \
         audio_codec, audio_channels, edition, bit_depth, network, quality_score, \
         is_dubbed, is_complete, is_remastered, is_scene, is_proper, is_repack, \
         platform, has_subtitles, is_anime, music_format, classified_at, classifier_version) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, \
                 $17, $18, $19, $20, $21, $22, $23, $24, $25, $26, $27, $28, $29, $30, NOW(), '0.1.0') \
         ON CONFLICT (info_hash) DO UPDATE SET \
           content_type = EXCLUDED.content_type, title = EXCLUDED.title, year = EXCLUDED.year, \
           quality_score = EXCLUDED.quality_score, classified_at = NOW()"
    )
    .bind(info_hash)
    .bind(&classification.content_type)
    .bind(&parsed.title)
    .bind(parsed.year)
    .bind(parsed.season)
    .bind(parsed.episode)
    .bind(&parsed.group)
    .bind(parsed.languages.first().map(|s| s.as_str()))
    .bind(&parsed.resolution)
    .bind(&parsed.codec)
    .bind(&parsed.video_source)
    .bind(&parsed.modifier)
    .bind(parsed.is_3d)
    .bind(parsed.hdr.first().map(|s| s.as_str()))
    .bind(parsed.audio_codecs.first().map(|s| s.as_str()))
    .bind(&parsed.audio_channels)
    .bind(&parsed.edition)
    .bind(&parsed.bit_depth)
    .bind(&parsed.network)
    .bind(quality_score)
    .bind(parsed.is_dubbed)
    .bind(parsed.is_complete)
    .bind(parsed.is_remastered)
    .bind(parsed.is_scene)
    .bind(parsed.is_proper)
    .bind(parsed.is_repack)
    .bind(classification.platform.or(parsed.platform.clone()))
    .bind(classification.has_subtitles || parsed.has_subtitles)
    .bind(classification.is_anime)
    .bind(&classification.music_format)
    .execute(pool)
    .await?;

    // Store tags
    for tag in &classification.tags {
        let _ = sqlx::query(
            "INSERT INTO torrent_tags (info_hash, tag, source) VALUES ($1, $2, 'classifier') \
             ON CONFLICT (info_hash, tag) DO NOTHING",
        )
        .bind(info_hash)
        .bind(tag)
        .execute(pool)
        .await;
    }

    tracing::debug!(
        hash = %info_hash,
        name = %meta.name,
        content_type = %classification.content_type,
        quality = quality_score,
        "resolved torrent"
    );

    Ok(())
}
