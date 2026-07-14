use std::net::SocketAddr;
use std::sync::Arc;

use dht::{Dht, DhtConfig, DhtState, Id20};
use tokio_util::sync::CancellationToken;

use crate::DhtSharedState;

/// Manages N DHT instances spread across the 160-bit keyspace.
pub struct DhtEngine {
    instances: Vec<Dht>,
    shared: Arc<DhtSharedState>,
    cancel: CancellationToken,
}

impl DhtEngine {
    /// Create a new DHT engine with N instances on sequential ports.
    pub async fn new(
        instance_count: u32,
        base_port: u16,
        shared: Arc<DhtSharedState>,
        cancel: CancellationToken,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let mut instances = Vec::with_capacity(instance_count as usize);

        for i in 0..instance_count {
            let port = base_port + (i as u16);
            let node_id = generate_spread_id(i, instance_count);

            let config = DhtConfig {
                peer_id: Some(node_id),
                listen_addr: Some(SocketAddr::from(([0, 0, 0, 0], port))),
                cancellation_token: Some(cancel.clone()),
                ..Default::default()
            };

            match DhtState::with_config(config).await {
                Ok(dht) => {
                    tracing::info!(instance = i, port, id = %hex::encode(node_id.0), "DHT instance started");
                    instances.push(dht);
                }
                Err(e) => {
                    tracing::error!(instance = i, port, error = %e, "failed to start DHT instance");
                    return Err(e.into());
                }
            }
        }

        tracing::info!(count = instances.len(), "DHT engine started");

        Ok(Self {
            instances,
            shared,
            cancel,
        })
    }

    /// Get the number of running instances.
    pub fn instance_count(&self) -> usize {
        self.instances.len()
    }

    /// Get combined routing table stats.
    pub fn stats(&self) -> EngineStats {
        let mut total_nodes = 0;
        for dht in &self.instances {
            let s = dht.stats();
            total_nodes += s.routing_table_size;
        }
        EngineStats {
            instances: self.instances.len(),
            total_routing_nodes: total_nodes,
            hash_queue_size: self.shared.hash_queue.lock().len(),
            peer_cache_size: self.shared.peer_cache.len(),
        }
    }

    /// Snapshot known IPv4 routing nodes for protocols, such as BEP 51,
    /// which are not implemented by the underlying DHT client.
    pub fn routing_nodes_v4(&self) -> Vec<SocketAddr> {
        self.instances
            .iter()
            .flat_map(|dht| {
                dht.with_routing_tables(|v4, _| {
                    v4.iter().map(|node| node.addr()).collect::<Vec<_>>()
                })
            })
            .collect()
    }

    /// Get a reference to a DHT instance for peer discovery.
    pub fn get_instance(&self, index: usize) -> Option<&Dht> {
        self.instances.get(index)
    }

    /// Discover peers for specific info_hashes by querying DHT concurrently.
    ///
    /// All hashes are queried in parallel (one future per hash, fanned across
    /// DHT instances round-robin). Previously this was sequential: N hashes ×
    /// 2 s/hash = up to 80 s per batch for N=40.
    pub async fn discover_peers(
        &self,
        info_hashes: &[String],
    ) -> dashmap::DashMap<String, Vec<SocketAddr>> {
        use futures::stream::{FuturesUnordered, StreamExt as FuturesStreamExt};

        let results = dashmap::DashMap::new();
        if self.instances.is_empty() || info_hashes.is_empty() {
            return results;
        }

        let instance_count = self.instances.len();
        type DiscoverFut =
            std::pin::Pin<Box<dyn std::future::Future<Output = (String, Vec<SocketAddr>)> + Send>>;
        let mut futs: FuturesUnordered<DiscoverFut> = FuturesUnordered::new();

        for (i, hash_hex) in info_hashes.iter().enumerate() {
            if self.cancel.is_cancelled() {
                break;
            }

            let Ok(hash_bytes) = hex::decode(hash_hex) else {
                continue;
            };
            if hash_bytes.len() != 20 {
                continue;
            }

            let mut id_bytes = [0u8; 20];
            id_bytes.copy_from_slice(&hash_bytes);
            let id = Id20::new(id_bytes);

            // Dht = Arc<DhtState>, so clone is just an Arc ref-count bump.
            let dht = self.instances[i % instance_count].clone();
            let hash_hex = hash_hex.clone();

            futs.push(Box::pin(async move {
                let mut stream = dht.get_peers(id, None);
                let mut peers = Vec::new();
                let timeout = tokio::time::sleep(std::time::Duration::from_secs(2));
                tokio::pin!(timeout);
                loop {
                    tokio::select! {
                        peer = tokio_stream::StreamExt::next(&mut stream) => match peer {
                            Some(addr) => {
                                peers.push(addr);
                                if peers.len() >= 15 { break; }
                            }
                            None => break,
                        },
                        () = &mut timeout => break,
                    }
                }
                (hash_hex, peers)
            }));
        }

        while let Some((hash, peers)) = futs.next().await {
            if !peers.is_empty() {
                results.insert(hash, peers);
            }
        }

        results
    }
}

pub struct EngineStats {
    pub instances: usize,
    pub total_routing_nodes: usize,
    pub hash_queue_size: usize,
    pub peer_cache_size: usize,
}

/// Generate a node ID spread across the 160-bit keyspace.
fn generate_spread_id(index: u32, total: u32) -> Id20 {
    let mut bytes: [u8; 20] = rand::random();
    if total <= 16 {
        let nibble = ((index as u8) * (16 / total.max(1) as u8)).min(15);
        bytes[0] = (nibble << 4) | (bytes[0] & 0x0F);
    } else {
        bytes[0] = ((index as u64 * 256 / total as u64) as u8).wrapping_add(bytes[0] & 0x0F);
    }
    Id20::new(bytes)
}
