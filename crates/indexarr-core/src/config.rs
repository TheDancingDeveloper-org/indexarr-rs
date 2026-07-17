use std::path::PathBuf;

use clap::ValueEnum;
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DbBackend {
    Postgresql,
    Sqlite,
}

impl std::fmt::Display for DbBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Postgresql => write!(f, "postgresql"),
            Self::Sqlite => write!(f, "sqlite"),
        }
    }
}

/// All configuration for Indexarr, loaded from INDEXARR_* env vars.
#[derive(Debug, Clone)]
pub struct Settings {
    // General
    pub debug: bool,
    pub data_dir: PathBuf,

    // Database
    pub db_backend: DbBackend,
    pub db_url: String,
    pub db_echo: bool,

    // DHT engine
    pub dht_instances: u32,
    pub dht_base_port: u16,
    pub dht_enable_bep51: bool,
    pub dht_crawl_interval: u64,
    pub peer_refresh_interval: u64,
    pub peer_refresh_batch: u32,

    // Resolver
    pub resolve_workers: u32,
    pub resolve_timeout: f64,
    pub save_files_threshold: u32,

    // Announcer
    pub announcer_enabled: bool,
    pub announcer_pool_size: u32,
    pub announcer_poll_interval: u64,
    pub announcer_settle_time: u64,
    pub announcer_rotate_interval: u64,
    pub announcer_port: u16,
    pub announcer_download_nfo: bool,

    // VPN / Proxy
    pub proxy_url: String,

    // Default trackers
    pub default_trackers: Vec<String>,

    // Content pipeline
    pub classifier_path: Option<PathBuf>,
    pub content_ban_enabled: bool,

    // TMDB
    pub tmdb_api_key: String,
    pub tmdb_rate_limit: f64,
    pub tmdb_enabled: bool,

    // HTTP server
    pub host: String,
    pub port: u16,
    pub sync_api_port: u16,

    // Torznab
    pub torznab_api_key: String,

    // P2P sync
    pub sync_enabled: bool,
    pub sync_export_interval: u64,
    pub sync_import_interval: u64,
    pub sync_discovery_interval: u64,
    pub sync_peers: Vec<String>,
    pub sync_lt_port: u16,
    pub sync_dht_port: u16,
    pub sync_dht_enabled: bool,
    pub sync_retention_days: u32,
    pub sync_max_delta_size: u32,
    pub sync_import_categories: Vec<String>,
    pub gossip_fanout: u32,
    pub gossip_max_peers: u32,
    pub sync_export_snapshots: bool,
    pub sync_verify_tls: bool,
    pub sync_push_enabled: bool,
    pub sync_delta_cache_mb: u32,
    pub sync_accept_push: bool,
    pub sync_external_url: String,
    pub sync_external_scheme: String,
    pub sync_external_port: u16,

    // Peer reputation
    pub sync_reputation_initial: f64,
    pub sync_reputation_trusted: f64,
    pub sync_reputation_untrusted: f64,
    pub sync_reputation_max: f64,
    pub sync_reputation_penalty_failure: f64,
    pub sync_reputation_penalty_invalid: f64,
    pub sync_reputation_bonus_contribution: f64,
    pub sync_reputation_bonus_per_day: f64,

    // Reachability check
    pub sync_reachability_check: bool,

    // XMPP
    pub xmpp_enabled: bool,
    pub xmpp_jid: String,
    pub xmpp_password: String,
    pub xmpp_server: String,
    pub xmpp_muc_room: String,

    // Swarm identity
    pub swarm_maintainer_pubkey: String,
    pub contributor_recovery_key: String,

    // Workers
    pub workers: Vec<String>,
}

impl Settings {
    /// Load settings from INDEXARR_* environment variables with defaults.
    pub fn from_env() -> Self {
        Self {
            debug: env_bool("INDEXARR_DEBUG", false),
            data_dir: env_path("INDEXARR_DATA_DIR", "data"),

            db_backend: env_db_backend("INDEXARR_DB_BACKEND", DbBackend::Postgresql),
            db_url: env_str(
                "INDEXARR_DB_URL",
                "postgres://indexarr:indexarr@localhost:5432/indexarr",
            ),
            db_echo: env_bool("INDEXARR_DB_ECHO", false),

            dht_instances: env_u32("INDEXARR_DHT_INSTANCES", 4),
            dht_base_port: env_u16("INDEXARR_DHT_BASE_PORT", 6881),
            dht_enable_bep51: env_bool("INDEXARR_DHT_ENABLE_BEP51", true),
            dht_crawl_interval: env_u64("INDEXARR_DHT_CRAWL_INTERVAL", 30),
            peer_refresh_interval: env_u64("INDEXARR_PEER_REFRESH_INTERVAL", 300),
            peer_refresh_batch: env_u32("INDEXARR_PEER_REFRESH_BATCH", 100),

            resolve_workers: env_u32("INDEXARR_RESOLVE_WORKERS", 20),
            resolve_timeout: env_f64("INDEXARR_RESOLVE_TIMEOUT", 15.0),
            save_files_threshold: env_u32("INDEXARR_SAVE_FILES_THRESHOLD", 200),

            announcer_enabled: env_bool("INDEXARR_ANNOUNCER_ENABLED", true),
            announcer_pool_size: env_u32("INDEXARR_ANNOUNCER_POOL_SIZE", 1000),
            announcer_poll_interval: env_u64("INDEXARR_ANNOUNCER_POLL_INTERVAL", 10),
            announcer_settle_time: env_u64("INDEXARR_ANNOUNCER_SETTLE_TIME", 45),
            announcer_rotate_interval: env_u64("INDEXARR_ANNOUNCER_ROTATE_INTERVAL", 120),
            announcer_port: env_u16("INDEXARR_ANNOUNCER_PORT", 6891),
            announcer_download_nfo: env_bool("INDEXARR_ANNOUNCER_DOWNLOAD_NFO", true),

            proxy_url: env_str("INDEXARR_PROXY_URL", ""),

            default_trackers: env_list(
                "INDEXARR_DEFAULT_TRACKERS",
                &[
                    "udp://tracker.opentrackr.org:1337/announce",
                    "udp://open.stealth.si:80/announce",
                    "udp://tracker.torrent.eu.org:451/announce",
                    "udp://open.demonii.com:1337/announce",
                    "udp://explodie.org:6969/announce",
                ],
            ),

            classifier_path: env_opt_path("INDEXARR_CLASSIFIER_PATH"),
            content_ban_enabled: env_bool("INDEXARR_CONTENT_BAN_ENABLED", true),

            tmdb_api_key: env_str("INDEXARR_TMDB_API_KEY", ""),
            tmdb_rate_limit: env_f64("INDEXARR_TMDB_RATE_LIMIT", 2.5),
            tmdb_enabled: env_bool("INDEXARR_TMDB_ENABLED", false),

            host: env_str("INDEXARR_HOST", "0.0.0.0"),
            port: env_u16("INDEXARR_PORT", 8080),
            sync_api_port: env_u16("INDEXARR_SYNC_API_PORT", 0),

            torznab_api_key: env_str("INDEXARR_TORZNAB_API_KEY", ""),

            sync_enabled: env_bool("INDEXARR_SYNC_ENABLED", true),
            sync_export_interval: env_u64("INDEXARR_SYNC_EXPORT_INTERVAL", 3600),
            sync_import_interval: env_u64("INDEXARR_SYNC_IMPORT_INTERVAL", 300),
            sync_discovery_interval: env_u64("INDEXARR_SYNC_DISCOVERY_INTERVAL", 900),
            sync_peers: env_json_list_or(
                "INDEXARR_SYNC_PEERS",
                &["https://bootstrap.indexarr.net"],
            ),
            sync_lt_port: env_u16("INDEXARR_SYNC_LT_PORT", 6890),
            sync_dht_port: env_u16("INDEXARR_SYNC_DHT_PORT", 6895),
            sync_dht_enabled: env_bool("INDEXARR_SYNC_DHT_ENABLED", true),
            sync_retention_days: env_u32("INDEXARR_SYNC_RETENTION_DAYS", 7),
            sync_max_delta_size: env_u32("INDEXARR_SYNC_MAX_DELTA_SIZE", 10_000),
            sync_import_categories: env_csv("INDEXARR_SYNC_IMPORT_CATEGORIES"),
            gossip_fanout: env_u32("INDEXARR_GOSSIP_FANOUT", 5),
            gossip_max_peers: env_u32("INDEXARR_GOSSIP_MAX_PEERS", 200),
            sync_export_snapshots: env_bool("INDEXARR_SYNC_EXPORT_SNAPSHOTS", false),
            sync_verify_tls: env_bool("INDEXARR_SYNC_VERIFY_TLS", true),
            sync_push_enabled: env_bool("INDEXARR_SYNC_PUSH_ENABLED", true),
            sync_delta_cache_mb: env_u32("INDEXARR_SYNC_DELTA_CACHE_MB", 512),
            sync_accept_push: env_bool("INDEXARR_SYNC_ACCEPT_PUSH", true),
            sync_external_url: env_str("INDEXARR_SYNC_EXTERNAL_URL", ""),
            sync_external_scheme: env_str("INDEXARR_SYNC_EXTERNAL_SCHEME", "http"),
            // Zero means "use INDEXARR_PORT". This keeps binary installs and
            // CLI --port overrides aligned without Docker-specific defaults.
            sync_external_port: env_u16("INDEXARR_SYNC_EXTERNAL_PORT", 0),

            sync_reputation_initial: env_f64("INDEXARR_SYNC_REPUTATION_INITIAL", 100.0),
            sync_reputation_trusted: env_f64("INDEXARR_SYNC_REPUTATION_TRUSTED", 500.0),
            sync_reputation_untrusted: env_f64("INDEXARR_SYNC_REPUTATION_UNTRUSTED", 20.0),
            sync_reputation_max: env_f64("INDEXARR_SYNC_REPUTATION_MAX", 10000.0),
            sync_reputation_penalty_failure: env_f64(
                "INDEXARR_SYNC_REPUTATION_PENALTY_FAILURE",
                10.0,
            ),
            sync_reputation_penalty_invalid: env_f64(
                "INDEXARR_SYNC_REPUTATION_PENALTY_INVALID",
                100.0,
            ),
            sync_reputation_bonus_contribution: env_f64(
                "INDEXARR_SYNC_REPUTATION_BONUS_CONTRIBUTION",
                0.1,
            ),
            sync_reputation_bonus_per_day: env_f64("INDEXARR_SYNC_REPUTATION_BONUS_PER_DAY", 5.0),

            sync_reachability_check: env_bool("INDEXARR_SYNC_REACHABILITY_CHECK", true),

            xmpp_enabled: env_bool("INDEXARR_XMPP_ENABLED", true),
            xmpp_jid: env_str("INDEXARR_XMPP_JID", ""),
            xmpp_password: env_str("INDEXARR_XMPP_PASSWORD", ""),
            xmpp_server: env_str(
                "INDEXARR_XMPP_SERVER",
                "conference.indexarr.net:5222",
            ),
            xmpp_muc_room: env_str(
                "INDEXARR_XMPP_MUC_ROOM",
                "indexarr-sync@conference.indexarr.net",
            ),

            swarm_maintainer_pubkey: env_str("INDEXARR_SWARM_MAINTAINER_PUBKEY", ""),
            contributor_recovery_key: env_str("INDEXARR_CONTRIBUTOR_RECOVERY_KEY", ""),

            workers: env_csv_or(
                "INDEXARR_WORKERS",
                &[
                    "http_server",
                    "dht_crawler",
                    "resolver",
                    "announcer",
                    "sync",
                ],
            ),
        }
    }

    /// SQLite database URL derived from data_dir.
    pub fn sqlite_url(&self) -> String {
        let db_path = self.data_dir.join("indexarr.db");
        format!("sqlite:{}", db_path.display())
    }

    /// The effective database URL based on the selected backend.
    pub fn effective_db_url(&self) -> String {
        match self.db_backend {
            DbBackend::Sqlite => self.sqlite_url(),
            DbBackend::Postgresql => self.db_url.clone(),
        }
    }
}

// --- Env helper functions ---

fn env_str(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_bool(key: &str, default: bool) -> bool {
    std::env::var(key)
        .map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(default)
}

fn env_u16(key: &str, default: u16) -> u16 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_u32(key: &str, default: u32) -> u32 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_f64(key: &str, default: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_path(key: &str, default: &str) -> PathBuf {
    std::env::var(key)
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(default))
}

fn env_opt_path(key: &str) -> Option<PathBuf> {
    std::env::var(key).ok().map(PathBuf::from)
}

fn env_db_backend(key: &str, default: DbBackend) -> DbBackend {
    std::env::var(key)
        .ok()
        .and_then(|v| match v.to_lowercase().as_str() {
            "postgresql" | "postgres" => Some(DbBackend::Postgresql),
            "sqlite" => Some(DbBackend::Sqlite),
            _ => None,
        })
        .unwrap_or(default)
}

fn env_csv(key: &str) -> Vec<String> {
    std::env::var(key)
        .ok()
        .filter(|v| !v.is_empty())
        .map(|v| v.split(',').map(|s| s.trim().to_string()).collect())
        .unwrap_or_default()
}

fn env_csv_or(key: &str, defaults: &[&str]) -> Vec<String> {
    std::env::var(key)
        .ok()
        .filter(|v| !v.is_empty())
        .map(|v| v.split(',').map(|s| s.trim().to_string()).collect())
        .unwrap_or_else(|| defaults.iter().map(|s| (*s).to_string()).collect())
}

fn env_list(key: &str, defaults: &[&str]) -> Vec<String> {
    // Try JSON array first, then CSV
    if let Ok(val) = std::env::var(key) {
        if val.starts_with('[')
            && let Ok(list) = serde_json::from_str::<Vec<String>>(&val)
        {
            return list;
        }
        return val.split(',').map(|s| s.trim().to_string()).collect();
    }
    defaults.iter().map(|s| (*s).to_string()).collect()
}

fn env_json_list_or(key: &str, defaults: &[&str]) -> Vec<String> {
    std::env::var(key)
        .ok()
        .filter(|v| !v.is_empty())
        .and_then(|v| serde_json::from_str::<Vec<String>>(&v).ok())
        .unwrap_or_else(|| defaults.iter().map(|value| (*value).to_string()).collect())
}
