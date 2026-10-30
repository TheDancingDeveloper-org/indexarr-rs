use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::Utc;
use flate2::Compression;
use flate2::write::GzEncoder;
use indexarr_identity::ContributorIdentity;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};

use crate::merge::{MAGNET_FRAGMENT_PREFIXES, PLACEHOLDER_NAMES};

/// Manifest describing available deltas for a peer.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Manifest {
    pub contributor_id: String,
    pub epoch: i32,
    pub sequence: i64,
    pub index_size: i64,
    pub deltas: Vec<DeltaInfo>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DeltaInfo {
    pub sequence: i64,
    pub count: i64,
    pub content_hash: String,
    pub created_at: String,
}

/// Export incremental NDJSON deltas of resolved torrents.
pub struct DeltaExporter {
    data_dir: PathBuf,
    max_delta_size: u32,
}

impl DeltaExporter {
    pub fn new(data_dir: &Path, max_delta_size: u32) -> Self {
        let sync_dir = data_dir.join("sync");
        let _ = std::fs::create_dir_all(&sync_dir);
        Self {
            data_dir: data_dir.to_path_buf(),
            max_delta_size,
        }
    }

    /// Export a new delta of unsynced resolved torrents. Returns (path, content_hash, count).
    pub async fn export_delta(
        &self,
        pool: &PgPool,
        identity: &ContributorIdentity,
        epoch: i32,
    ) -> Result<Option<(PathBuf, String, i64)>, Box<dyn std::error::Error + Send + Sync>> {
        // Get current sequence
        let mut sequence = self.load_sequence();

        // Query unsynced resolved torrents
        let rows = sqlx::query(
            "SELECT t.info_hash, t.name, t.size, t.source, t.discovered_at, t.resolved_at, \
             t.seed_count, t.peer_count, t.nfo, t.trackers, t.announced_at, t.epoch, t.contributor_id, \
             c.content_type, c.title, c.year, c.season, c.episode, c.\"group\", c.language, \
             c.resolution, c.codec, c.video_source, c.modifier, c.is_3d, c.hdr, \
             c.audio_codec, c.audio_channels, c.tmdb_id, c.imdb_id, c.platform, \
             c.has_subtitles, c.is_anime, c.music_format, c.edition, c.bit_depth, \
             c.network, c.quality_score \
             FROM torrents t LEFT JOIN torrent_content c ON t.info_hash = c.info_hash \
             WHERE t.resolved_at IS NOT NULL AND t.seed_count >= 1 AND t.sync_sequence IS NULL \
             AND t.name IS NOT NULL \
             AND lower(btrim(t.name)) <> ALL($2) \
             AND NOT lower(btrim(t.name)) LIKE ANY($3) \
             ORDER BY t.resolved_at ASC LIMIT $1"
        )
        .bind(self.max_delta_size as i64)
        .bind(PLACEHOLDER_NAMES)
        .bind(
            MAGNET_FRAGMENT_PREFIXES
                .iter()
                .map(|prefix| format!("{prefix}%"))
                .collect::<Vec<_>>(),
        )
        .fetch_all(pool)
        .await?;

        if rows.is_empty() {
            return Ok(None);
        }

        let count = rows.len() as i64;
        sequence += 1;

        // Write gzip NDJSON
        let sync_dir = self.data_dir.join("sync");
        let filename = format!("delta_{sequence}.ndjson.gz");
        let path = sync_dir.join(&filename);

        let file = std::fs::File::create(&path)?;
        let mut gz = GzEncoder::new(file, Compression::new(6));
        let mut hasher = Sha256::new();

        for row in &rows {
            let info_hash: String = row.get("info_hash");
            let name: Option<String> = row.get("name");
            let size: Option<i64> = row.get("size");

            // Sign the record
            let signature = identity
                .sign_delta_meta(&info_hash, name.as_deref(), size, epoch)
                .unwrap_or_default();

            let ct: Option<String> = row.get("content_type");
            let content_val = if ct.is_some() {
                serde_json::json!({
                    "content_type": ct,
                    "title": row.get::<Option<String>, _>("title"),
                    "year": row.get::<Option<i32>, _>("year"),
                    "season": row.get::<Option<i32>, _>("season"),
                    "episode": row.get::<Option<i32>, _>("episode"),
                    "resolution": row.get::<Option<String>, _>("resolution"),
                    "codec": row.get::<Option<String>, _>("codec"),
                    "video_source": row.get::<Option<String>, _>("video_source"),
                    "platform": row.get::<Option<String>, _>("platform"),
                    "is_anime": row.get::<Option<bool>, _>("is_anime").unwrap_or(false),
                    "music_format": row.get::<Option<String>, _>("music_format"),
                    "quality_score": row.get::<Option<i32>, _>("quality_score"),
                })
            } else {
                serde_json::Value::Null
            };

            let record = serde_json::json!({
                "info_hash": info_hash,
                "name": name,
                "size": size,
                "source": row.get::<String, _>("source"),
                "seed_count": row.get::<i32, _>("seed_count"),
                "peer_count": row.get::<i32, _>("peer_count"),
                "nfo": row.get::<Option<String>, _>("nfo"),
                "trackers": row.get::<Option<serde_json::Value>, _>("trackers"),
                "announced_at": row.get::<Option<chrono::DateTime<chrono::Utc>>, _>("announced_at").map(|d| d.to_rfc3339()),
                "resolved_at": row.get::<Option<chrono::DateTime<chrono::Utc>>, _>("resolved_at").map(|d| d.to_rfc3339()),
                "content": content_val,
                "_meta": {
                    "sequence": sequence,
                    "exported_at": Utc::now().to_rfc3339(),
                    "epoch": epoch,
                    "contributor_id": identity.contributor_id().unwrap_or("unknown"),
                    "contributor_pubkey": identity.public_key_b64().unwrap_or_default(),
                    "signature": signature,
                }
            });

            let line = serde_json::to_string(&record)?;
            gz.write_all(line.as_bytes())?;
            gz.write_all(b"\n")?;
            hasher.update(line.as_bytes());
            hasher.update(b"\n");
        }

        gz.finish()?;
        let content_hash = hex::encode(hasher.finalize());

        // Mark exported torrents with sequence
        let hashes: Vec<String> = rows
            .iter()
            .map(|r| r.get::<String, _>("info_hash"))
            .collect();
        for chunk in hashes.chunks(500) {
            sqlx::query("UPDATE torrents SET sync_sequence = $1 WHERE info_hash = ANY($2)")
                .bind(sequence)
                .bind(chunk)
                .execute(pool)
                .await?;
        }

        // Save sequence
        self.save_sequence(sequence);

        tracing::info!(sequence, count, hash = %content_hash, "exported delta");
        Ok(Some((path, content_hash, count)))
    }

    /// Build a manifest of all available deltas.
    pub fn build_manifest(&self, contributor_id: &str, epoch: i32) -> Manifest {
        let sequence = self.load_sequence();
        let sync_dir = self.data_dir.join("sync");

        let mut deltas = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&sync_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("gz")
                    && let Some(name) = path.file_stem().and_then(|n| n.to_str())
                    && name.starts_with("delta_")
                {
                    // Compute content hash
                    if let Ok(data) = std::fs::read(&path) {
                        let hash = hex::encode(Sha256::digest(&data));
                        deltas.push(DeltaInfo {
                            sequence: name
                                .strip_prefix("delta_")
                                .and_then(|s| s.strip_suffix(".ndjson"))
                                .and_then(|s| s.parse().ok())
                                .unwrap_or(0),
                            count: 0,
                            content_hash: hash,
                            created_at: Utc::now().to_rfc3339(),
                        });
                    }
                }
            }
        }

        deltas.sort_by_key(|d| d.sequence);

        Manifest {
            contributor_id: contributor_id.to_string(),
            epoch,
            sequence,
            index_size: 0,
            deltas,
        }
    }

    fn load_sequence(&self) -> i64 {
        let path = self.data_dir.join("sync").join("sequence");
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    }

    fn save_sequence(&self, seq: i64) {
        let path = self.data_dir.join("sync").join("sequence");
        let _ = std::fs::write(path, seq.to_string());
    }
}

/// Read an NDJSON delta file (gzipped) and return parsed records.
pub fn read_delta(
    path: &Path,
) -> Result<Vec<serde_json::Value>, Box<dyn std::error::Error + Send + Sync>> {
    let file = std::fs::File::open(path)?;
    let decoder = flate2::read::GzDecoder::new(file);
    let reader = std::io::BufReader::new(decoder);

    use std::io::BufRead;
    let mut records = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.is_empty() {
            continue;
        }
        let record: serde_json::Value = serde_json::from_str(&line)?;
        records.push(record);
    }

    Ok(records)
}
