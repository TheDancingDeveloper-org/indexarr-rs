use std::collections::VecDeque;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use dht::Id20;
use indexarr_bep51::{
    COMPACT_NODE_V4_LEN, SampleInfohashesArgs, SampleInfohashesRespOwned, decode_response,
    encode_query, iter_samples, validate_response,
};
use tokio::net::UdpSocket;
use tokio_util::sync::CancellationToken;

use crate::engine::DhtEngine;
use crate::{DhtSharedState, DiscoveredHash};

/// Well-known DHT bootstrap nodes — used to seed the BEP 51 sampler's node
/// queue on startup and after queue exhaustion.
const BOOTSTRAP: &[&str] = &[
    "dht.transmissionbt.com:6881",
    "dht.libtorrent.org:25401",
    "router.utorrent.com:6881",
    "router.bittorrent.com:6881",
];

/// Maximum number of nodes held in the sampler's query queue.
const MAX_QUEUE: usize = 5_000;

/// Delay between individual sample_infohashes queries to avoid flooding the
/// DHT network.
const INTER_QUERY_MS: u64 = 100;

/// Per-query response timeout.
const QUERY_TIMEOUT: Duration = Duration::from_secs(5);

/// Run the BEP 51 DHT infohash sampler.
///
/// Opens its own UDP socket and periodically issues `sample_infohashes` KRPC
/// queries (BEP 51) to known DHT nodes, feeding discovered info_hashes into
/// `shared` and expanding the node queue from each response's `nodes` field.
pub async fn run_bep51_sampler(
    shared: Arc<DhtSharedState>,
    engine: Arc<DhtEngine>,
    cancel: CancellationToken,
) {
    // Wait for the DHT to warm up before we start hammering nodes.
    tokio::select! {
        () = tokio::time::sleep(Duration::from_secs(45)) => {}
        () = cancel.cancelled() => return,
    }

    let socket = match UdpSocket::bind("0.0.0.0:0").await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "BEP 51 sampler: failed to bind UDP socket");
            return;
        }
    };
    tracing::info!(
        addr = %socket.local_addr().unwrap_or_else(|_| "0.0.0.0:0".parse().unwrap()),
        "BEP 51 sampler started"
    );

    let my_id = Id20::new(rand::random());
    let mut node_queue: VecDeque<SocketAddr> = VecDeque::new();
    let mut txn_counter: u16 = 0;
    let mut recv_buf = vec![0u8; 4096];

    seed_queue(&mut node_queue, &engine).await;

    let mut total_samples: u64 = 0;

    loop {
        if cancel.is_cancelled() {
            break;
        }

        // Refill queue from bootstrap if exhausted.
        if node_queue.is_empty() {
            tracing::debug!("BEP 51 sampler: node queue empty, reseeding");
            seed_queue(&mut node_queue, &engine).await;
            if node_queue.is_empty() {
                tokio::time::sleep(Duration::from_secs(10)).await;
                continue;
            }
        }

        let node = match node_queue.pop_front() {
            Some(n) => n,
            None => continue,
        };

        txn_counter = txn_counter.wrapping_add(1);
        let txn_id = txn_counter.to_be_bytes();
        let target = Id20::new(rand::random());
        let args = SampleInfohashesArgs { id: my_id, target };

        let query = match encode_query(&txn_id, &args) {
            Ok(q) => q,
            Err(e) => {
                tracing::trace!(error = %e, "BEP 51: encode_query failed");
                continue;
            }
        };

        if socket.send_to(&query, node).await.is_err() {
            continue;
        }

        let recv_result =
            tokio::time::timeout(QUERY_TIMEOUT, socket.recv_from(&mut recv_buf)).await;

        if let Ok(Ok((len, src))) = recv_result {
            if src != node {
                tracing::trace!(expected = %node, actual = %src, "BEP 51: ignored response from unexpected node");
                continue;
            }

            match decode_matching_response(&recv_buf[..len], &txn_id) {
                Ok(resp) => {
                    // Push discovered info_hashes into the shared ingest queue.
                    let mut count = 0usize;
                    for hash_bytes in iter_samples(resp.samples.as_ref()) {
                        let hex = hex::encode(hash_bytes);
                        shared.push_hash(DiscoveredHash {
                            info_hash: hex,
                            peer_ip: None,
                            peer_port: None,
                            source: "bep51".to_string(),
                        });
                        count += 1;
                    }
                    total_samples += count as u64;

                    // Expand the node queue from the response's compact node list.
                    let nodes = resp.nodes.as_ref();
                    let node_count = nodes.len() / COMPACT_NODE_V4_LEN;
                    for i in 0..node_count {
                        let base = i * COMPACT_NODE_V4_LEN + 20; // skip 20-byte node id
                        if base + 6 > nodes.len() {
                            break;
                        }
                        let ip = Ipv4Addr::new(
                            nodes[base],
                            nodes[base + 1],
                            nodes[base + 2],
                            nodes[base + 3],
                        );
                        let port = u16::from_be_bytes([nodes[base + 4], nodes[base + 5]]);
                        if port > 0 && node_queue.len() < MAX_QUEUE {
                            node_queue.push_back(SocketAddr::from((ip, port)));
                        }
                    }

                    if count > 0 {
                        tracing::debug!(
                            samples = count,
                            nodes = node_count,
                            total = total_samples,
                            "BEP 51: response"
                        );
                    }
                }
                // Most failures here are error responses from nodes that
                // don't implement BEP 51 — safe to ignore.
                Err(e) => tracing::trace!(error = %e, "BEP 51: decode error"),
            }
        }

        tokio::time::sleep(Duration::from_millis(INTER_QUERY_MS)).await;
    }

    tracing::info!(total_samples, "BEP 51 sampler stopped");
}

/// Decode and validate the response to one in-flight query.
///
/// UDP transaction IDs are the only wire-level association between a query
/// and response. Accepting a different transaction can attribute unrelated
/// sample data to the current crawl.
fn decode_matching_response(
    buf: &[u8],
    expected_txn: &[u8],
) -> Result<SampleInfohashesRespOwned, indexarr_bep51::Bep51Error> {
    let (txn, response) = decode_response(buf)?;
    if txn.as_ref() != expected_txn {
        return Err(indexarr_bep51::Bep51Error::TransactionMismatch);
    }
    validate_response(&response)?;
    Ok(response)
}

async fn seed_queue(queue: &mut VecDeque<SocketAddr>, engine: &DhtEngine) {
    // The well-known bootstrap routers are primarily a way to enter the DHT;
    // they need not implement BEP 51 themselves. Seed from the warmed-up
    // librtbit routing tables so the sampler reaches ordinary network nodes.
    for addr in engine.routing_nodes_v4() {
        if queue.len() >= MAX_QUEUE {
            return;
        }
        if !queue.contains(&addr) {
            queue.push_back(addr);
        }
    }

    for host in BOOTSTRAP {
        match tokio::net::lookup_host(host).await {
            Ok(addrs) => {
                for addr in addrs {
                    if queue.len() < MAX_QUEUE && !queue.contains(&addr) {
                        queue.push_back(addr);
                    }
                }
            }
            Err(e) => tracing::trace!(host, error = %e, "BEP 51: bootstrap lookup failed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use bencode::ByteBufOwned;
    use indexarr_bep51::{SampleInfohashesResp, encode_response};

    use super::*;

    fn response(samples: Vec<u8>) -> Vec<u8> {
        encode_response(
            b"tx",
            &SampleInfohashesResp {
                id: Id20::new([3; 20]),
                interval: 60,
                nodes: ByteBufOwned::default(),
                nodes6: None,
                num: 1,
                samples: ByteBufOwned::from(samples),
            },
        )
        .unwrap()
    }

    #[test]
    fn crawler_ingests_sampled_hash_not_random_query_target() {
        let query_target = [7; 20];
        let sampled_hash = [9; 20];
        let decoded = decode_matching_response(&response(sampled_hash.to_vec()), b"tx").unwrap();
        let hashes: Vec<&[u8]> = iter_samples(decoded.samples.as_ref()).collect();

        assert_eq!(hashes, vec![sampled_hash.as_slice()]);
        assert!(!hashes.contains(&query_target.as_slice()));
    }

    #[test]
    fn crawler_rejects_response_for_another_transaction() {
        let error = decode_matching_response(&response([[9; 20]].concat()), b"other").unwrap_err();

        assert!(matches!(
            error,
            indexarr_bep51::Bep51Error::TransactionMismatch
        ));
    }
}
