use std::path::Path;

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use indexarr_identity::{BanList, verify_delta_signature};

use crate::epoch;

/// Statistics from a merge operation.
#[derive(Debug, Clone, Default)]
pub struct MergeStats {
    pub inserted: u64,
    pub updated: u64,
    pub skipped: u64,
    pub errors: u64,
    pub signature_failures: u64,
}

const MAX_FILES_PER_TORRENT: usize = 5000;
const MAX_TAGS_PER_TORRENT: usize = 100;
#[allow(dead_code)]
const MAX_COMMENTS_PER_TORRENT: usize = 500;
#[allow(dead_code)]
const MAX_VOTES_PER_TORRENT: usize = 5000;

/// Lowercased names that say nothing about the torrent. The empty string
/// covers blank names, because names are trimmed before comparing.
pub(crate) const PLACEHOLDER_NAMES: &[&str] = &["", "invalid magnet", "magnet"];

/// Lowercased prefixes of names that are pieces of a magnet URI, not titles.
pub(crate) const MAGNET_FRAGMENT_PREFIXES: &[&str] = &["magnet:?", "&tr=", "&dn="];

/// True when a torrent name is missing, a placeholder or part of a magnet URI.
///
/// `DeltaExporter::export_delta` applies the same rule in SQL.
pub(crate) fn is_placeholder_name(name: Option<&str>) -> bool {
    let Some(name) = name else {
        return true;
    };
    let name = name.trim().to_lowercase();
    PLACEHOLDER_NAMES.contains(&name.as_str())
        || MAGNET_FRAGMENT_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
}

/// Merge a delta file into the local database.
pub async fn merge_delta(
    pool: &PgPool,
    delta_path: &Path,
    data_dir: &Path,
    ban_list: &BanList,
    import_categories: &[String],
    _bulk_insert: bool,
) -> Result<MergeStats, Box<dyn std::error::Error + Send + Sync>> {
    let records = crate::delta::read_delta(delta_path)?;
    let mut stats = MergeStats::default();

    for record in &records {
        match merge_record(pool, record, data_dir, ban_list, import_categories).await {
            Ok(action) => match action {
                MergeAction::Inserted => stats.inserted += 1,
                MergeAction::Updated => stats.updated += 1,
                MergeAction::Skipped => stats.skipped += 1,
                MergeAction::SignatureFailed => stats.signature_failures += 1,
            },
            Err(e) => {
                tracing::debug!(error = %e, "merge record error");
                stats.errors += 1;
            }
        }
    }

    tracing::info!(
        inserted = stats.inserted,
        updated = stats.updated,
        skipped = stats.skipped,
        sig_failures = stats.signature_failures,
        errors = stats.errors,
        "delta merge complete"
    );

    Ok(stats)
}

enum MergeAction {
    Inserted,
    Updated,
    Skipped,
    SignatureFailed,
}

async fn merge_record(
    pool: &PgPool,
    record: &serde_json::Value,
    data_dir: &Path,
    ban_list: &BanList,
    import_categories: &[String],
) -> Result<MergeAction, Box<dyn std::error::Error + Send + Sync>> {
    let info_hash = record
        .get("info_hash")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if info_hash.is_empty() || info_hash.len() != 40 {
        return Ok(MergeAction::Skipped);
    }

    let meta = record.get("_meta").unwrap_or(&serde_json::Value::Null);
    let record_epoch = meta.get("epoch").and_then(|v| v.as_i64()).unwrap_or(1) as i32;
    let contributor_id = meta
        .get("contributor_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let pubkey = meta
        .get("contributor_pubkey")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let signature = meta.get("signature").and_then(|v| v.as_str()).unwrap_or("");

    // Epoch check
    if !epoch::is_epoch_acceptable(data_dir, record_epoch) {
        return Ok(MergeAction::Skipped);
    }

    // Ban check
    if ban_list.is_banned(contributor_id) {
        return Ok(MergeAction::Skipped);
    }

    // Seed-only mode check
    if epoch::in_seed_only_mode(data_dir) && !epoch::is_seed_contributor(data_dir, contributor_id) {
        return Ok(MergeAction::Skipped);
    }

    // Skip private torrents
    if record
        .get("private")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return Ok(MergeAction::Skipped);
    }

    // Category filter
    if !import_categories.is_empty() {
        let ct = record
            .get("content")
            .and_then(|c| c.get("content_type"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if !ct.is_empty() && !import_categories.iter().any(|c| c == ct) {
            return Ok(MergeAction::Skipped);
        }
    }

    // Signature verification
    let name = record.get("name").and_then(|v| v.as_str());
    let size = record.get("size").and_then(|v| v.as_i64());
    if !pubkey.is_empty()
        && !signature.is_empty()
        && !verify_delta_signature(pubkey, signature, info_hash, name, size, record_epoch)
    {
        return Ok(MergeAction::SignatureFailed);
    }

    // Placeholder names would otherwise be stored as resolved and never fetched
    if is_placeholder_name(name) {
        return Ok(MergeAction::Skipped);
    }

    // Check if torrent exists
    let existing: Option<(Option<DateTime<Utc>>,)> =
        sqlx::query_as("SELECT resolved_at FROM torrents WHERE info_hash = $1")
            .bind(info_hash)
            .fetch_optional(pool)
            .await?;

    match existing {
        None => {
            // New torrent — insert
            insert_torrent(pool, info_hash, record, meta).await?;
            Ok(MergeAction::Inserted)
        }
        Some((None,)) => {
            // Existing but unresolved — update
            update_unresolved(pool, info_hash, record).await?;
            Ok(MergeAction::Updated)
        }
        Some((Some(_),)) => {
            // Existing and resolved — fill gaps only
            fill_gaps(pool, info_hash, record).await?;
            Ok(MergeAction::Updated)
        }
    }
}

async fn insert_torrent(
    pool: &PgPool,
    info_hash: &str,
    record: &serde_json::Value,
    meta: &serde_json::Value,
) -> Result<(), sqlx::Error> {
    let name = record.get("name").and_then(|v| v.as_str());
    let size = record.get("size").and_then(|v| v.as_i64());
    let seed_count = record
        .get("seed_count")
        .and_then(|v| v.as_i64())
        .unwrap_or(1)
        .max(1) as i32;
    let peer_count = record
        .get("peer_count")
        .and_then(|v| v.as_i64())
        .unwrap_or(0) as i32;
    let nfo = record.get("nfo").and_then(|v| v.as_str());
    let trackers = record.get("trackers");
    let epoch = meta.get("epoch").and_then(|v| v.as_i64()).unwrap_or(1) as i32;
    let contributor_id = meta.get("contributor_id").and_then(|v| v.as_str());

    sqlx::query(
        "INSERT INTO torrents (info_hash, name, size, source, resolved_at, announced_at, \
         seed_count, peer_count, nfo, trackers, epoch, contributor_id) \
         VALUES ($1, $2, $3, 'sync', NOW(), NOW(), $4, $5, $6, $7, $8, $9) \
         ON CONFLICT (info_hash) DO NOTHING",
    )
    .bind(info_hash)
    .bind(name)
    .bind(size)
    .bind(seed_count)
    .bind(peer_count)
    .bind(nfo)
    .bind(trackers)
    .bind(epoch)
    .bind(contributor_id)
    .execute(pool)
    .await?;

    // Insert content
    if let Some(content) = record.get("content")
        && !content.is_null()
    {
        let _ = sqlx::query(
            "INSERT INTO torrent_content (info_hash, content_type, title, year, season, episode, \
             resolution, codec, video_source, platform, is_anime, music_format, quality_score) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) \
             ON CONFLICT (info_hash) DO NOTHING",
        )
        .bind(info_hash)
        .bind(content.get("content_type").and_then(|v| v.as_str()))
        .bind(content.get("title").and_then(|v| v.as_str()))
        .bind(
            content
                .get("year")
                .and_then(|v| v.as_i64())
                .map(|v| v as i32),
        )
        .bind(
            content
                .get("season")
                .and_then(|v| v.as_i64())
                .map(|v| v as i32),
        )
        .bind(
            content
                .get("episode")
                .and_then(|v| v.as_i64())
                .map(|v| v as i32),
        )
        .bind(content.get("resolution").and_then(|v| v.as_str()))
        .bind(content.get("codec").and_then(|v| v.as_str()))
        .bind(content.get("video_source").and_then(|v| v.as_str()))
        .bind(content.get("platform").and_then(|v| v.as_str()))
        .bind(
            content
                .get("is_anime")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        )
        .bind(content.get("music_format").and_then(|v| v.as_str()))
        .bind(
            content
                .get("quality_score")
                .and_then(|v| v.as_i64())
                .map(|v| v as i32),
        )
        .execute(pool)
        .await;
    }

    // Insert files (limited)
    if let Some(files) = record.get("files").and_then(|v| v.as_array()) {
        for file in files.iter().take(MAX_FILES_PER_TORRENT) {
            let path = file.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let size = file.get("size").and_then(|v| v.as_i64()).unwrap_or(0);
            let ext = file.get("extension").and_then(|v| v.as_str());
            let _ = sqlx::query(
                "INSERT INTO torrent_files (info_hash, path, size, extension) VALUES ($1, $2, $3, $4)"
            )
            .bind(info_hash).bind(path).bind(size).bind(ext)
            .execute(pool).await;
        }
    }

    // Insert tags (limited)
    if let Some(tags) = record.get("tags").and_then(|v| v.as_array()) {
        for tag in tags.iter().take(MAX_TAGS_PER_TORRENT) {
            let tag_val = tag.get("tag").and_then(|v| v.as_str()).unwrap_or("");
            if !tag_val.is_empty() {
                let _ = sqlx::query(
                    "INSERT INTO torrent_tags (info_hash, tag, source) VALUES ($1, $2, 'sync') \
                     ON CONFLICT (info_hash, tag) DO NOTHING",
                )
                .bind(info_hash)
                .bind(tag_val)
                .execute(pool)
                .await;
            }
        }
    }

    Ok(())
}

async fn update_unresolved(
    pool: &PgPool,
    info_hash: &str,
    record: &serde_json::Value,
) -> Result<(), sqlx::Error> {
    let name = record.get("name").and_then(|v| v.as_str());
    let size = record.get("size").and_then(|v| v.as_i64());
    let seed_count = record
        .get("seed_count")
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
        .max(1) as i32;
    let peer_count = record
        .get("peer_count")
        .and_then(|v| v.as_i64())
        .unwrap_or(0) as i32;

    sqlx::query(
        "UPDATE torrents SET \
           name = COALESCE($2, name), \
           size = COALESCE($3, size), \
           resolved_at = NOW(), \
           seed_count = GREATEST(seed_count, $4), \
           peer_count = GREATEST(peer_count, $5), \
           no_peers = FALSE \
         WHERE info_hash = $1",
    )
    .bind(info_hash)
    .bind(name)
    .bind(size)
    .bind(seed_count)
    .bind(peer_count)
    .execute(pool)
    .await?;

    Ok(())
}

async fn fill_gaps(
    pool: &PgPool,
    info_hash: &str,
    record: &serde_json::Value,
) -> Result<(), sqlx::Error> {
    let seed_count = record
        .get("seed_count")
        .and_then(|v| v.as_i64())
        .unwrap_or(0) as i32;
    let peer_count = record
        .get("peer_count")
        .and_then(|v| v.as_i64())
        .unwrap_or(0) as i32;

    // Only fill: max seed/peer, fill nfo if missing
    sqlx::query(
        "UPDATE torrents SET \
           seed_count = GREATEST(seed_count, $2), \
           peer_count = GREATEST(peer_count, $3), \
           nfo = COALESCE(nfo, $4) \
         WHERE info_hash = $1",
    )
    .bind(info_hash)
    .bind(seed_count)
    .bind(peer_count)
    .bind(record.get("nfo").and_then(|v| v.as_str()))
    .execute(pool)
    .await?;

    // Union-merge tags
    if let Some(tags) = record.get("tags").and_then(|v| v.as_array()) {
        for tag in tags.iter().take(MAX_TAGS_PER_TORRENT) {
            let tag_val = tag.get("tag").and_then(|v| v.as_str()).unwrap_or("");
            if !tag_val.is_empty() {
                let _ = sqlx::query(
                    "INSERT INTO torrent_tags (info_hash, tag, source) VALUES ($1, $2, 'sync') \
                     ON CONFLICT (info_hash, tag) DO NOTHING",
                )
                .bind(info_hash)
                .bind(tag_val)
                .execute(pool)
                .await;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::is_placeholder_name;

    #[test]
    fn rejects_missing_and_placeholder_names() {
        for name in [
            None,
            Some(""),
            Some("   "),
            Some("Invalid Magnet"),
            Some("  invalid magnet "),
            Some("Magnet"),
            Some("&tr="),
            Some("&tr=udp://tracker.example:1337/announce"),
            Some("&dn=Some.Title"),
            Some("magnet:?xt=urn:btih:0f1d98fb1ea2edd1365bdf102fb67069cf3233d9"),
        ] {
            assert!(is_placeholder_name(name), "{name:?} should be rejected");
        }
    }

    #[test]
    fn keeps_real_titles() {
        for name in [
            "The.Voice.S17E17.WEB.x264-TRUMP",
            "Magnet.Man.2024.1080p.WEB",
            "Invalid Magnet Theory (2019)",
            "S01",
        ] {
            assert!(!is_placeholder_name(Some(name)), "{name:?} should be kept");
        }
    }
}
