//! Tracker-driven peer discovery — announces against a curated list of
//! popular public BitTorrent trackers (HTTP + UDP) to harvest peer
//! addresses for a given info_hash.
//!
//! Used by `MetadataResolver` as a complement to DHT-based peer discovery:
//! many DHT-discovered peers are stale or don't carry metadata, but
//! tracker-discovered peers are typically active seeders/leechers more
//! likely to share the info dict.
//!
//! Layered on `librtbit-tracker-comms::TrackerComms::start`, which returns
//! a stream of peer `SocketAddr`s and handles HTTP/UDP transport, retries,
//! and per-tracker `interval` honoring internally.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use librtbit_core::hash_id::Id20;
use tokio_util::sync::CancellationToken;
use tracker_comms::{TorrentStatsProvider, TrackerComms, TrackerCommsStats, UdpTrackerClient};
use url::Url;

/// Curated public-tracker list — the most reliable open trackers as of
/// 2026-04. Subset of ngosang/trackerslist's "best" tier; UDP first because
/// they're cheaper (no TCP handshake) and most public trackers are UDP-only
/// these days.
///
/// Override at runtime via [`TrackerDiscovery::with_trackers`] if you want
/// to add private trackers or trim the list.
pub const POPULAR_PUBLIC_TRACKERS: &[&str] = &[
    "udp://tracker.opentrackr.org:1337/announce",
    "udp://open.demonii.com:1337/announce",
    "udp://open.stealth.si:80/announce",
    "udp://exodus.desync.com:6969/announce",
    "udp://tracker.torrent.eu.org:451/announce",
    "udp://tracker.openbittorrent.com:6969/announce",
    "udp://tracker.dler.org:6969/announce",
    "udp://opentracker.i2p.rocks:6969/announce",
    "udp://explodie.org:6969/announce",
    "http://tracker.openbittorrent.com:80/announce",
];

/// "I have nothing, am downloading nothing" — the announce stats we send.
/// We're an indexer, not a participant, so we always advertise zero.
struct ZeroStats;
impl TorrentStatsProvider for ZeroStats {
    fn get(&self) -> TrackerCommsStats {
        TrackerCommsStats::default()
    }
}

/// Process-wide tracker discovery client. Holds a single `UdpTrackerClient`
/// (one UDP socket, shared across all info_hashes) and a single
/// `reqwest::Client` for HTTP trackers.
pub struct TrackerDiscovery {
    udp_client: UdpTrackerClient,
    reqwest_client: reqwest::Client,
    peer_id: Id20,
    /// Port we advertise to trackers. Trackers refuse `port=0`, so this
    /// must be set to something non-zero — using our DHT base port is fine
    /// (it's the address we'd want peers to connect to anyway, not that
    /// peers ever do for an indexer).
    announce_port: u16,
    trackers: HashSet<Url>,
}

impl TrackerDiscovery {
    pub async fn new(
        cancel: CancellationToken,
        peer_id: Id20,
        announce_port: u16,
    ) -> anyhow::Result<Self> {
        let udp_client = UdpTrackerClient::new(cancel, None).await?;
        let reqwest_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()?;
        let trackers = POPULAR_PUBLIC_TRACKERS
            .iter()
            .filter_map(|s| Url::parse(s).ok())
            .collect();
        Ok(Self {
            udp_client,
            reqwest_client,
            peer_id,
            announce_port,
            trackers,
        })
    }

    /// Replace the default tracker list. Pass an empty iterator to disable
    /// tracker discovery (the resolver falls back to DHT-only).
    #[allow(dead_code)]
    pub fn with_trackers<I, S>(&mut self, trackers: I)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.trackers = trackers
            .into_iter()
            .filter_map(|s| Url::parse(s.as_ref()).ok())
            .collect();
    }

    /// Announce against every configured tracker for `info_hash`, collect
    /// peer addresses for up to `timeout`, return the deduped set.
    ///
    /// `TrackerComms::start` returns a long-lived stream that re-announces
    /// at each tracker's stated interval. We poll it for `timeout` to
    /// capture the first round of responses, then drop the stream — the
    /// tasks inside cancel cleanly via `FuturesUnordered` Drop.
    pub async fn discover_peers(&self, info_hash: Id20, timeout: Duration) -> Vec<SocketAddr> {
        if self.trackers.is_empty() {
            return Vec::new();
        }

        let stream = TrackerComms::start(
            info_hash,
            self.peer_id,
            self.trackers.clone(),
            Box::new(ZeroStats),
            None, // honor the tracker's stated interval (we drop stream before reannounce)
            self.announce_port,
            self.reqwest_client.clone(),
            Some(self.udp_client.clone()),
            None, // per-tracker status registry is not needed for one-shot discovery
        );
        let Some(mut stream) = stream else {
            return Vec::new();
        };

        let mut peers: HashSet<SocketAddr> = HashSet::new();
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            match tokio::time::timeout_at(deadline, stream.next()).await {
                Ok(Some(addr)) => {
                    peers.insert(addr);
                }
                Ok(None) => break, // stream ended (all trackers errored / drained)
                Err(_) => break,   // overall timeout
            }
        }
        peers.into_iter().collect()
    }
}

/// Convenience alias — most consumers want a shared instance behind `Arc`.
pub type SharedTrackerDiscovery = Arc<TrackerDiscovery>;
