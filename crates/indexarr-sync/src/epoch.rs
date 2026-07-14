use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use indexarr_identity::verify_signature;

/// Epoch declaration — signed by maintainer to trigger data purges.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpochDeclaration {
    pub epoch: i32,
    pub reason: String,
    pub effective_at: DateTime<Utc>,
    pub seed_contributors: Vec<String>,
    pub seed_only_hours: f64,
    pub signature: String,
}

/// Persisted epoch state.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EpochState {
    pub last_seen_epoch: i32,
    pub transition_at: Option<DateTime<Utc>>,
    pub purge_completed: bool,
}

/// Load the current epoch declaration from disk.
pub fn get_declaration(data_dir: &Path) -> Option<EpochDeclaration> {
    let path = data_dir.join("sync").join("epoch_declaration.json");
    let data = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

/// Get the current epoch number.
pub fn get_current_epoch(data_dir: &Path) -> i32 {
    get_declaration(data_dir).map(|d| d.epoch).unwrap_or(1)
}

/// Check if a delta's epoch is acceptable (strict: current only).
pub fn is_epoch_acceptable(data_dir: &Path, delta_epoch: i32) -> bool {
    delta_epoch == get_current_epoch(data_dir)
}

/// Check if a contributor is in the seed list for current epoch.
pub fn is_seed_contributor(data_dir: &Path, contributor_id: &str) -> bool {
    get_declaration(data_dir)
        .map(|d| d.seed_contributors.contains(&contributor_id.to_string()))
        .unwrap_or(false)
}

/// Check if we're in seed-only mode (within seed_only_hours of transition).
pub fn in_seed_only_mode(data_dir: &Path) -> bool {
    let decl = match get_declaration(data_dir) {
        Some(d) if d.seed_only_hours > 0.0 => d,
        _ => return false,
    };

    let state = load_state(data_dir);
    let transition_at = match state.transition_at {
        Some(t) => t,
        None => return false,
    };

    let elapsed_hours = (Utc::now() - transition_at).num_seconds() as f64 / 3600.0;
    elapsed_hours < decl.seed_only_hours
}

/// Verify an epoch declaration's signature.
pub fn verify_declaration(decl: &EpochDeclaration, maintainer_pubkey: &str) -> bool {
    if maintainer_pubkey.is_empty() {
        return false;
    }

    let mut seed_sorted = decl.seed_contributors.clone();
    seed_sorted.sort();
    let payload = format!(
        "epoch_declaration:{}:{}:{}:{}",
        decl.epoch,
        decl.effective_at.to_rfc3339(),
        seed_sorted.join(","),
        decl.seed_only_hours,
    );

    verify_signature(maintainer_pubkey, &decl.signature, payload.as_bytes())
}

/// Apply an epoch declaration (transition or bootstrap adoption).
pub async fn apply_epoch_declaration(
    decl: &EpochDeclaration,
    data_dir: &Path,
    pool: &sqlx::PgPool,
    contributor_id: &str,
    maintainer_pubkey: &str,
    adopt: bool,
) -> Result<(), String> {
    let current = get_current_epoch(data_dir);
    if decl.epoch <= current {
        return Err(format!(
            "epoch {} not newer than current {}",
            decl.epoch, current
        ));
    }

    if !verify_declaration(decl, maintainer_pubkey) {
        return Err("invalid maintainer signature".to_string());
    }

    // Save declaration
    let sync_dir = data_dir.join("sync");
    let _ = std::fs::create_dir_all(&sync_dir);
    let decl_json = serde_json::to_string_pretty(decl).map_err(|e| e.to_string())?;
    std::fs::write(sync_dir.join("epoch_declaration.json"), decl_json)
        .map_err(|e| e.to_string())?;

    if adopt {
        // Bootstrap adoption: no purge, no transition timestamp
        save_state(
            data_dir,
            &EpochState {
                last_seen_epoch: decl.epoch,
                transition_at: None,
                purge_completed: true,
            },
        );
        tracing::info!(epoch = decl.epoch, "adopted epoch declaration (bootstrap)");
        return Ok(());
    }

    // Purge data
    let is_seed = decl.seed_contributors.contains(&contributor_id.to_string());
    if is_seed {
        // Seed node: only purge sync-imported data
        tracing::info!(epoch = decl.epoch, "seed node: purging sync-imported data");
        let _ = sqlx::query("DELETE FROM torrent_content WHERE info_hash IN (SELECT info_hash FROM torrents WHERE source = 'sync')").execute(pool).await;
        let _ = sqlx::query("DELETE FROM torrent_files WHERE info_hash IN (SELECT info_hash FROM torrents WHERE source = 'sync')").execute(pool).await;
        let _ = sqlx::query("DELETE FROM torrent_tags WHERE info_hash IN (SELECT info_hash FROM torrents WHERE source = 'sync')").execute(pool).await;
        let _ = sqlx::query("DELETE FROM torrent_comments WHERE info_hash IN (SELECT info_hash FROM torrents WHERE source = 'sync')").execute(pool).await;
        let _ = sqlx::query("DELETE FROM torrent_votes WHERE info_hash IN (SELECT info_hash FROM torrents WHERE source = 'sync')").execute(pool).await;
        let _ = sqlx::query("DELETE FROM torrents WHERE source = 'sync'")
            .execute(pool)
            .await;
        let _ = sqlx::query(
            "UPDATE torrents SET sync_sequence = NULL, epoch = $1 WHERE source != 'sync'",
        )
        .bind(decl.epoch)
        .execute(pool)
        .await;
    } else {
        // Non-seed: purge everything
        tracing::info!(epoch = decl.epoch, "non-seed node: purging all data");
        let _ = sqlx::query("DELETE FROM torrent_content")
            .execute(pool)
            .await;
        let _ = sqlx::query("DELETE FROM torrent_files").execute(pool).await;
        let _ = sqlx::query("DELETE FROM torrent_tags").execute(pool).await;
        let _ = sqlx::query("DELETE FROM torrent_comments")
            .execute(pool)
            .await;
        let _ = sqlx::query("DELETE FROM torrent_votes").execute(pool).await;
        let _ = sqlx::query("DELETE FROM torrents").execute(pool).await;
    }

    // Reset peer watermarks
    let _ = sqlx::query("UPDATE sync_state SET last_sequence = 0")
        .execute(pool)
        .await;

    // Clean filesystem
    clean_sync_dir(&sync_dir);

    save_state(
        data_dir,
        &EpochState {
            last_seen_epoch: decl.epoch,
            transition_at: Some(Utc::now()),
            purge_completed: true,
        },
    );

    tracing::info!(epoch = decl.epoch, is_seed, "epoch transition complete");
    Ok(())
}

fn load_state(data_dir: &Path) -> EpochState {
    let path = data_dir.join("sync").join("epoch_state.json");
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_state(data_dir: &Path, state: &EpochState) {
    let sync_dir = data_dir.join("sync");
    let _ = std::fs::create_dir_all(&sync_dir);
    if let Ok(json) = serde_json::to_string_pretty(state) {
        let _ = std::fs::write(sync_dir.join("epoch_state.json"), json);
    }
}

fn clean_sync_dir(sync_dir: &Path) {
    if let Ok(entries) = std::fs::read_dir(sync_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(ext) = path.extension().and_then(|e| e.to_str())
                && matches!(ext, "ndjson" | "gz" | "torrent")
            {
                let _ = std::fs::remove_file(&path);
            }
            if path.file_name().map(|n| n == "sequence").unwrap_or(false) {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}
