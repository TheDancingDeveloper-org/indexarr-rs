use std::collections::HashMap;
use std::net::IpAddr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};

/// Information about a sync peer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerInfo {
    pub peer_id: String,
    pub url: String,
    pub source: String,
    pub last_seen: DateTime<Utc>,
    pub last_sequence: i64,
    pub fail_count: i32,
    pub first_seen: DateTime<Utc>,
    pub reputation: f64,
}

impl PeerInfo {
    pub fn is_healthy(&self, untrusted_threshold: f64) -> bool {
        self.reputation > untrusted_threshold
    }

    pub fn is_trusted(&self, trusted_threshold: f64) -> bool {
        self.reputation >= trusted_threshold
    }
}

/// Whether a URL learned from another node can reach a remote peer.
///
/// Wildcard and loopback addresses are only meaningful on the node that
/// advertised them. Dialing one here reaches this node, so it would end up
/// merging its own deltas.
fn is_dialable_peer_url(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return false;
    }
    let Some(host) = parsed.host_str() else {
        return false;
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    match host.parse::<IpAddr>() {
        Ok(ip) => {
            let ip = ip.to_canonical();
            !ip.is_unspecified() && !ip.is_loopback()
        }
        Err(_) => {
            let host = host.to_ascii_lowercase();
            host != "localhost" && !host.ends_with(".localhost")
        }
    }
}

/// Manages the sync peer table with reputation tracking and tiered eviction.
pub struct PeerTable {
    peers: HashMap<String, PeerInfo>,
    url_index: HashMap<String, String>,
    local_contributor_id: Option<String>,
    max_peers: usize,
    reputation_initial: f64,
    reputation_max: f64,
    reputation_untrusted: f64,
    penalty_failure: f64,
    bonus_contribution: f64,
    bonus_per_day: f64,
}

impl PeerTable {
    pub fn new(
        max_peers: usize,
        reputation_initial: f64,
        reputation_max: f64,
        reputation_untrusted: f64,
        penalty_failure: f64,
        bonus_contribution: f64,
        bonus_per_day: f64,
    ) -> Self {
        Self {
            peers: HashMap::new(),
            url_index: HashMap::new(),
            local_contributor_id: None,
            max_peers,
            reputation_initial,
            reputation_max,
            reputation_untrusted,
            penalty_failure,
            bonus_contribution,
            bonus_per_day,
        }
    }

    /// Set this node's contributor ID so discovery never adds it as a peer.
    pub fn set_local_contributor(&mut self, contributor_id: Option<String>) {
        self.local_contributor_id = contributor_id;
    }

    /// Whether a discovered peer is usable. Bootstrap peers come from local
    /// configuration, so an operator can still point one at a local address.
    fn accepts_peer(&self, peer_id: &str, url: &str, source: &str) -> bool {
        source == "bootstrap"
            || (self.local_contributor_id.as_deref() != Some(peer_id) && is_dialable_peer_url(url))
    }

    /// Add or update a peer. Returns false if the peer is this node or
    /// unreachable, or if the table is full and can't evict.
    pub fn add_peer(&mut self, peer_id: &str, url: &str, source: &str) -> bool {
        if !self.accepts_peer(peer_id, url, source) {
            tracing::debug!(peer_id, url, source, "ignoring unusable sync peer");
            return false;
        }

        // Dedup by URL
        if self.url_index.contains_key(url) {
            return true;
        }

        if self.peers.contains_key(peer_id) {
            // Update existing
            if let Some(peer) = self.peers.get_mut(peer_id) {
                peer.url = url.to_string();
                peer.last_seen = Utc::now();
            }
            return true;
        }

        // Evict if at capacity
        if self.peers.len() >= self.max_peers && !self.evict_one() {
            return false;
        }

        let now = Utc::now();
        self.url_index.insert(url.to_string(), peer_id.to_string());
        self.peers.insert(
            peer_id.to_string(),
            PeerInfo {
                peer_id: peer_id.to_string(),
                url: url.to_string(),
                source: source.to_string(),
                last_seen: now,
                last_sequence: 0,
                fail_count: 0,
                first_seen: now,
                reputation: self.reputation_initial,
            },
        );
        true
    }

    /// Evict the lowest-reputation non-bootstrap peer.
    fn evict_one(&mut self) -> bool {
        let victim = self
            .peers
            .iter()
            .filter(|(_, p)| p.source != "bootstrap")
            .min_by(|a, b| {
                a.1.reputation
                    .partial_cmp(&b.1.reputation)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(id, _)| id.clone());

        if let Some(id) = victim {
            if let Some(peer) = self.peers.remove(&id) {
                self.url_index.remove(&peer.url);
            }
            true
        } else {
            false
        }
    }

    /// Get gossip targets weighted by reputation.
    pub fn get_gossip_targets(&self, count: usize) -> Vec<&PeerInfo> {
        let mut healthy: Vec<&PeerInfo> = self
            .peers
            .values()
            .filter(|p| p.is_healthy(self.reputation_untrusted))
            .collect();

        // Sort by reputation descending, take top N
        healthy.sort_by(|a, b| {
            b.reputation
                .partial_cmp(&a.reputation)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Ensure at least one bootstrap peer if available
        let has_bootstrap = healthy.iter().any(|p| p.source == "bootstrap");
        if !has_bootstrap && let Some(bp) = self.peers.values().find(|p| p.source == "bootstrap") {
            healthy.insert(0, bp);
        }

        healthy.into_iter().take(count).collect()
    }

    /// Update health after gossip attempt.
    pub fn update_health(&mut self, peer_id: &str, success: bool) {
        let (should_remove, penalty) = {
            let Some(peer) = self.peers.get_mut(peer_id) else {
                return;
            };
            if success {
                peer.fail_count = 0;
                peer.last_seen = Utc::now();
                return;
            }
            peer.fail_count += 1;
            let should_remove = peer.fail_count >= 10 && peer.source != "bootstrap";
            (should_remove, self.penalty_failure)
        };

        self.apply_reputation_delta(peer_id, -penalty);

        if should_remove && let Some(peer) = self.peers.remove(peer_id) {
            self.url_index.remove(&peer.url);
        }
    }

    /// Update the watermark (highest sequence seen from this peer).
    pub fn update_watermark(&mut self, peer_id: &str, sequence: i64) {
        if let Some(peer) = self.peers.get_mut(peer_id) {
            peer.last_sequence = peer.last_sequence.max(sequence);
        }
    }

    /// Apply contribution bonus for valid records merged.
    pub fn apply_contribution_bonus(&mut self, peer_id: &str, record_count: u64) {
        let bonus = self.bonus_contribution * record_count as f64;
        self.apply_reputation_delta(peer_id, bonus);
    }

    /// Apply longevity bonus (called each discovery round).
    pub fn apply_longevity_bonus(&mut self, discovery_interval_secs: u64) {
        let fraction = discovery_interval_secs as f64 / 86400.0;
        let bonus = self.bonus_per_day * fraction;

        let ids: Vec<String> = self.peers.keys().cloned().collect();
        for id in ids {
            self.apply_reputation_delta(&id, bonus);
        }
    }

    fn apply_reputation_delta(&mut self, peer_id: &str, delta: f64) {
        if let Some(peer) = self.peers.get_mut(peer_id) {
            peer.reputation = (peer.reputation + delta).clamp(0.0, self.reputation_max);
        }
    }

    pub fn peer_count(&self) -> usize {
        self.peers.len()
    }

    pub fn healthy_count(&self) -> usize {
        self.peers
            .values()
            .filter(|p| p.is_healthy(self.reputation_untrusted))
            .count()
    }

    pub fn peers(&self) -> impl Iterator<Item = &PeerInfo> {
        self.peers.values()
    }

    /// Persist peer table to database.
    pub async fn persist(&self, pool: &PgPool) -> Result<(), sqlx::Error> {
        // Clear existing
        sqlx::query("DELETE FROM sync_state").execute(pool).await?;

        for peer in self.peers.values() {
            sqlx::query(
                "INSERT INTO sync_state (peer_id, peer_url, last_sync_at, last_sequence, \
                 first_seen, fail_count, reputation, source) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            )
            .bind(&peer.peer_id)
            .bind(&peer.url)
            .bind(peer.last_seen)
            .bind(peer.last_sequence)
            .bind(peer.first_seen)
            .bind(peer.fail_count)
            .bind(peer.reputation)
            .bind(&peer.source)
            .execute(pool)
            .await?;
        }

        Ok(())
    }

    /// Load peer table from database.
    pub async fn load(&mut self, pool: &PgPool) -> Result<(), sqlx::Error> {
        let rows = sqlx::query("SELECT * FROM sync_state")
            .fetch_all(pool)
            .await?;
        for row in &rows {
            let peer_id: String = row.get("peer_id");
            let url: String = row.get::<Option<String>, _>("peer_url").unwrap_or_default();
            if url.is_empty() {
                continue;
            }

            let source: String = row.get("source");
            if !self.accepts_peer(&peer_id, &url, &source) {
                continue;
            }

            let peer = PeerInfo {
                peer_id: peer_id.clone(),
                url: url.clone(),
                source,
                last_seen: row
                    .get::<Option<DateTime<Utc>>, _>("last_sync_at")
                    .unwrap_or_else(Utc::now),
                last_sequence: row.get("last_sequence"),
                fail_count: row.get("fail_count"),
                first_seen: row
                    .get::<Option<DateTime<Utc>>, _>("first_seen")
                    .unwrap_or_else(Utc::now),
                reputation: row.get("reputation"),
            };

            self.url_index.insert(url, peer_id.clone());
            self.peers.insert(peer_id, peer);
        }

        tracing::info!(peers = self.peers.len(), "loaded peer table from DB");
        Ok(())
    }

    /// Add bootstrap peers from configuration.
    pub fn add_bootstrap_peers(&mut self, urls: &[String]) {
        for url in urls {
            let peer_id = format!("bootstrap-{}", &sha256_hex(url.as_bytes())[..8]);
            self.add_peer(&peer_id, url, "bootstrap");
        }
    }
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(data))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> PeerTable {
        let mut table = PeerTable::new(10, 100.0, 10000.0, 20.0, 10.0, 0.1, 5.0);
        table.set_local_contributor(Some("TN-self".to_string()));
        table
    }

    #[test]
    fn local_addresses_are_not_dialable() {
        for url in [
            "http://0.0.0.0:8080",
            "http://0:8080",
            "http://127.0.0.1:8080",
            "http://127.1.2.3:8080",
            "http://[::]:8080",
            "http://[::1]:8080",
            "http://[::ffff:127.0.0.1]:8080",
            "http://localhost:8080",
            "http://LOCALHOST:8080",
            "http://node.localhost:8080",
        ] {
            assert!(!is_dialable_peer_url(url), "{url}");
        }
    }

    #[test]
    fn malformed_urls_are_not_dialable() {
        for url in ["", "not a url", "ftp://peer.example:21", "http://"] {
            assert!(!is_dialable_peer_url(url), "{url}");
        }
    }

    #[test]
    fn remote_addresses_are_dialable() {
        for url in [
            "https://bootstrap.indexarr.net",
            "http://198.51.100.20:8080",
            "http://100.64.0.1:8080",
            "http://192.168.1.20:8080",
            "http://[2001:db8::2]:8080",
            "http://localhost.example:8080",
        ] {
            assert!(is_dialable_peer_url(url), "{url}");
        }
    }

    #[test]
    fn discovery_skips_local_urls_and_this_node() {
        let mut table = table();
        assert!(!table.add_peer("TN-other", "http://0.0.0.0:8080", "pex"));
        assert!(!table.add_peer("TN-other", "http://127.0.0.1:8080", "xmpp"));
        assert!(!table.add_peer("TN-self", "http://203.0.113.10:8080", "pex"));
        assert_eq!(table.peer_count(), 0);

        assert!(table.add_peer("TN-other", "http://198.51.100.20:8080", "pex"));
        assert_eq!(table.peer_count(), 1);
    }

    #[test]
    fn configured_bootstrap_peers_may_be_local() {
        let mut table = table();
        table.add_bootstrap_peers(&["http://localhost:8080".to_string()]);
        assert_eq!(table.peer_count(), 1);
    }

    #[test]
    fn unusable_peer_cannot_replace_a_known_url() {
        let mut table = table();
        assert!(table.add_peer("TN-other", "http://198.51.100.20:8080", "pex"));
        assert!(!table.add_peer("TN-other", "http://0.0.0.0:8080", "pex"));
        assert_eq!(
            table.peers().next().unwrap().url,
            "http://198.51.100.20:8080"
        );
    }
}
