//! End-to-end checks against a scratch PostgreSQL database. These write to the
//! database, so they are ignored by default and need their own URL:
//!
//! ```bash
//! INDEXARR_TEST_DB_URL=postgres://... cargo test -p indexarr-sync -- --ignored --test-threads=1
//! ```

use std::io::Write;
use std::path::PathBuf;

use flate2::Compression;
use flate2::write::GzEncoder;
use indexarr_core::config::{DbBackend, Settings};
use indexarr_core::db::init_db;
use indexarr_identity::{BanList, ContributorIdentity};
use indexarr_sync::delta::{DeltaExporter, read_delta};
use indexarr_sync::merge::merge_delta;
use sqlx::PgPool;

const GOOD: &str = "aa00000000000000000000000000000000000001";
const GOOD_SIZE_ZERO: &str = "aa00000000000000000000000000000000000002";
const INVALID_MAGNET: &str = "aa00000000000000000000000000000000000003";
const MAGNET: &str = "aa00000000000000000000000000000000000004";
const FRAGMENT: &str = "aa00000000000000000000000000000000000005";
const NO_NAME: &str = "aa00000000000000000000000000000000000006";
const UNRESOLVED: &str = "aa00000000000000000000000000000000000007";

async fn scratch_pool() -> PgPool {
    let mut settings = Settings::from_env();
    settings.db_backend = DbBackend::Postgresql;
    settings.db_url = std::env::var("INDEXARR_TEST_DB_URL").expect("INDEXARR_TEST_DB_URL");
    let pool = init_db(&settings).await.expect("connect to scratch db");
    sqlx::query("TRUNCATE torrents CASCADE")
        .execute(&pool)
        .await
        .expect("empty scratch db");
    pool
}

fn scratch_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("indexarr-sync-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn write_delta(path: &PathBuf, records: &[serde_json::Value]) {
    let file = std::fs::File::create(path).expect("create delta");
    let mut gz = GzEncoder::new(file, Compression::default());
    for record in records {
        writeln!(gz, "{record}").expect("write record");
    }
    gz.finish().expect("finish delta");
}

fn record(info_hash: &str, name: serde_json::Value, size: i64) -> serde_json::Value {
    serde_json::json!({
        "info_hash": info_hash,
        "name": name,
        "size": size,
        "seed_count": 1,
        "peer_count": 0,
        "_meta": { "epoch": 1, "contributor_id": "TN-test" },
    })
}

#[tokio::test]
#[ignore = "needs INDEXARR_TEST_DB_URL pointing at a scratch database"]
async fn merge_skips_placeholder_names() {
    let pool = scratch_pool().await;
    let dir = scratch_dir("merge");
    sqlx::query("INSERT INTO torrents (info_hash, source) VALUES ($1, 'bep51')")
        .bind(UNRESOLVED)
        .execute(&pool)
        .await
        .expect("insert unresolved row");

    let delta = dir.join("delta.ndjson.gz");
    write_delta(
        &delta,
        &[
            record(GOOD, "The.Voice.S17E17.WEB.x264-TRUMP".into(), 1000),
            record(GOOD_SIZE_ZERO, "The.Voice.S11E13.HDTV.x264".into(), 0),
            record(INVALID_MAGNET, "Invalid Magnet".into(), 0),
            record(MAGNET, "Magnet".into(), 1),
            record(FRAGMENT, "&tr=".into(), 0),
            record(NO_NAME, serde_json::Value::Null, 0),
            record(UNRESOLVED, "Invalid Magnet".into(), 0),
        ],
    );

    let ban_list = BanList::new("", &dir);
    let stats = merge_delta(&pool, &delta, &dir, &ban_list, &[], false)
        .await
        .expect("merge");
    assert_eq!((stats.inserted, stats.skipped), (2, 5));

    let hashes: Vec<(String,)> =
        sqlx::query_as("SELECT info_hash FROM torrents WHERE source = 'sync' ORDER BY info_hash")
            .fetch_all(&pool)
            .await
            .expect("read sync rows");
    let hashes: Vec<&str> = hashes.iter().map(|(hash,)| hash.as_str()).collect();
    assert_eq!(hashes, [GOOD, GOOD_SIZE_ZERO]);

    // The local unresolved row must stay queued for the resolver
    let (name, resolved): (Option<String>, bool) =
        sqlx::query_as("SELECT name, resolved_at IS NOT NULL FROM torrents WHERE info_hash = $1")
            .bind(UNRESOLVED)
            .fetch_one(&pool)
            .await
            .expect("read unresolved row");
    assert_eq!((name, resolved), (None, false));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
#[ignore = "needs INDEXARR_TEST_DB_URL pointing at a scratch database"]
async fn export_leaves_out_placeholder_names() {
    let pool = scratch_pool().await;
    let dir = scratch_dir("export");
    for (hash, name, size) in [
        (GOOD, Some("The.Voice.S17E17.WEB.x264-TRUMP"), 1000_i64),
        (GOOD_SIZE_ZERO, Some("The.Voice.S11E13.HDTV.x264"), 0),
        (INVALID_MAGNET, Some("Invalid Magnet"), 0),
        (MAGNET, Some(" magnet "), 1),
        (FRAGMENT, Some("&tr=udp://tracker.example:1337/announce"), 0),
        (NO_NAME, None, 0),
    ] {
        sqlx::query(
            "INSERT INTO torrents (info_hash, name, size, source, resolved_at, seed_count) \
             VALUES ($1, $2, $3, 'sync', NOW(), 1)",
        )
        .bind(hash)
        .bind(name)
        .bind(size)
        .execute(&pool)
        .await
        .expect("insert row");
    }

    let mut identity = ContributorIdentity::new(&dir);
    identity.load_or_generate().expect("identity");
    let (path, _, count) = DeltaExporter::new(&dir, 100)
        .export_delta(&pool, &identity, 1)
        .await
        .expect("export")
        .expect("a delta");
    assert_eq!(count, 2);

    let exported = read_delta(&path).expect("read delta");
    let mut hashes: Vec<&str> = exported
        .iter()
        .filter_map(|record| record.get("info_hash")?.as_str())
        .collect();
    hashes.sort_unstable();
    assert_eq!(hashes, [GOOD, GOOD_SIZE_ZERO]);

    let _ = std::fs::remove_dir_all(&dir);
}
