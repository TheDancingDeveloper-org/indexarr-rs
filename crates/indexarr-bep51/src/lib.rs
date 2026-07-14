//! BEP 51 — DHT Infohash Indexing.
//!
//! Implements the `sample_infohashes` query and response defined in
//! [BEP 51](https://www.bittorrent.org/beps/bep_0051.html). Lives in
//! `indexarr-rs` as a standalone codec while it stabilises; once upstreamed
//! into `librtbit-dht` 0.2.0 this crate is retired (see `bep-uplift.md`).
//!
//! Wire format mirrored byte-for-byte against the Python reference
//! implementation in `btpydht` (`krcp.py:178-179, 388-406`,
//! `tests/test_bep51.py`).

use bencode::{ByteBufOwned, bencode_serialize_to_writer, from_bytes};
use librtbit_core::hash_id::Id20;
use serde::{Deserialize, Serialize};

/// Method name used in the `q` field of a sample_infohashes KRPC query.
pub const METHOD_NAME: &[u8] = b"sample_infohashes";

/// Per-spec maximum value for the `interval` field of a sample_infohashes
/// response (6 hours, in seconds).
pub const MAX_INTERVAL_SECS: u32 = 21_600;

/// Per-spec maximum number of 20-byte hashes that may appear in `samples`.
pub const MAX_SAMPLES: usize = 20;

/// Length of a single info_hash sample, in bytes.
pub const SAMPLE_LEN: usize = 20;

/// Length of one IPv4 compact node (20-byte id + 6-byte ip:port).
pub const COMPACT_NODE_V4_LEN: usize = 26;

/// Length of one IPv6 compact node (20-byte id + 18-byte ip:port).
pub const COMPACT_NODE_V6_LEN: usize = 38;

/// Arguments dict (`a`) for a `sample_infohashes` query.
///
/// Wire form: `{"id": <20-byte node id>, "target": <20-byte target id>}`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SampleInfohashesArgs {
    pub id: Id20,
    pub target: Id20,
}

/// Response dict (`r`) for a `sample_infohashes` reply.
///
/// All fields per BEP 51:
///   - `id`: responder node id
///   - `interval`: seconds the requester should wait before re-querying (≤ 21600)
///   - `nodes`: compact IPv4 node info, multiple of 26 bytes
///   - `nodes6`: compact IPv6 node info, multiple of 38 bytes (optional;
///     not in BEP 51 spec but emitted for consistency with BEP 32 deployments)
///   - `num`: total number of distinct info_hashes the responder is currently tracking
///   - `samples`: concatenation of up to 20 randomly-sampled 20-byte info_hashes
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SampleInfohashesResp<BufT = ByteBufOwned> {
    pub id: Id20,
    pub interval: u32,
    pub nodes: BufT,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nodes6: Option<BufT>,
    pub num: u32,
    pub samples: BufT,
}

/// Owned alias used by tests + downstream consumers.
pub type SampleInfohashesRespOwned = SampleInfohashesResp<ByteBufOwned>;

/// KRPC envelope for an outgoing query. Serialize-only — incoming queries
/// are decoded via [`decode_query`].
#[derive(Debug, Serialize)]
struct OutgoingQuery<'a> {
    #[serde(rename = "y", with = "y_q")]
    message_type: (),
    #[serde(rename = "t", with = "serde_bytes")]
    transaction_id: &'a [u8],
    #[serde(rename = "q", with = "serde_bytes")]
    method: &'a [u8],
    #[serde(rename = "a")]
    args: &'a SampleInfohashesArgs,
}

/// KRPC envelope for an outgoing response. Serialize-only.
#[derive(Debug, Serialize)]
struct OutgoingResponse<'a, BufT> {
    #[serde(rename = "y", with = "y_r")]
    message_type: (),
    #[serde(rename = "t", with = "serde_bytes")]
    transaction_id: &'a [u8],
    #[serde(rename = "r")]
    response: &'a SampleInfohashesResp<BufT>,
}

mod y_q {
    use serde::Serializer;
    pub fn serialize<S: Serializer>(_: &(), s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(b"q")
    }
}

mod y_r {
    use serde::Serializer;
    pub fn serialize<S: Serializer>(_: &(), s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(b"r")
    }
}

mod serde_bytes {
    use serde::Serializer;
    pub fn serialize<S: Serializer>(b: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(b)
    }
}

/// Errors raised by the BEP 51 codec.
#[derive(Debug, thiserror::Error)]
pub enum Bep51Error {
    #[error("bencode serialize: {0}")]
    Serialize(#[from] bencode::SerializeError),
    #[error("bencode deserialize: {0}")]
    Deserialize(String),
    #[error("samples field length {0} is not a multiple of 20")]
    SamplesNotMultipleOf20(usize),
    #[error("nodes field length {0} is not a multiple of 26")]
    NodesNotMultipleOf26(usize),
    #[error("interval {0} exceeds spec maximum {MAX_INTERVAL_SECS}")]
    IntervalTooLarge(u32),
    #[error("samples count {0} exceeds spec maximum {MAX_SAMPLES}")]
    TooManySamples(usize),
    #[error("missing required field: {0}")]
    MissingField(&'static str),
    #[error("response transaction id does not match the query")]
    TransactionMismatch,
}

/// Encode a `sample_infohashes` query to bencode.
pub fn encode_query(
    transaction_id: &[u8],
    args: &SampleInfohashesArgs,
) -> Result<Vec<u8>, Bep51Error> {
    let mut buf = Vec::new();
    let env = OutgoingQuery {
        message_type: (),
        transaction_id,
        method: METHOD_NAME,
        args,
    };
    bencode_serialize_to_writer(env, &mut buf)?;
    Ok(buf)
}

/// Encode a `sample_infohashes` response to bencode, validating spec invariants.
pub fn encode_response<BufT>(
    transaction_id: &[u8],
    resp: &SampleInfohashesResp<BufT>,
) -> Result<Vec<u8>, Bep51Error>
where
    BufT: AsRef<[u8]> + Serialize,
{
    validate_response(resp)?;
    let mut buf = Vec::new();
    let env = OutgoingResponse {
        message_type: (),
        transaction_id,
        response: resp,
    };
    bencode_serialize_to_writer(env, &mut buf)?;
    Ok(buf)
}

/// Validate response invariants per BEP 51.
pub fn validate_response<BufT: AsRef<[u8]>>(
    resp: &SampleInfohashesResp<BufT>,
) -> Result<(), Bep51Error> {
    let samples = resp.samples.as_ref();
    if !samples.len().is_multiple_of(SAMPLE_LEN) {
        return Err(Bep51Error::SamplesNotMultipleOf20(samples.len()));
    }
    if samples.len() / SAMPLE_LEN > MAX_SAMPLES {
        return Err(Bep51Error::TooManySamples(samples.len() / SAMPLE_LEN));
    }
    let nodes = resp.nodes.as_ref();
    if !nodes.len().is_multiple_of(COMPACT_NODE_V4_LEN) {
        return Err(Bep51Error::NodesNotMultipleOf26(nodes.len()));
    }
    if resp.interval > MAX_INTERVAL_SECS {
        return Err(Bep51Error::IntervalTooLarge(resp.interval));
    }
    Ok(())
}

/// Decode a KRPC query envelope and confirm it is a `sample_infohashes`
/// query, returning the args. Any other method name → `Ok(None)`.
///
/// Performs a two-stage decode: the first stage classifies the message
/// (cheap — it ignores the `a` dict via `serde::de::IgnoredAny`), and only
/// when the method name matches do we re-parse with the full
/// [`SampleInfohashesArgs`] schema. This way a `ping` query (whose `a` dict
/// has no `target` field) is a clean `Ok(None)` rather than an error.
pub fn decode_query(
    buf: &[u8],
) -> Result<Option<(ByteBufOwned, SampleInfohashesArgs)>, Bep51Error> {
    use serde::de::IgnoredAny;
    #[derive(Deserialize)]
    struct Header {
        #[serde(rename = "t")]
        t: ByteBufOwned,
        #[serde(rename = "q")]
        q: Option<ByteBufOwned>,
        #[serde(rename = "y")]
        y: Option<ByteBufOwned>,
        #[serde(rename = "a")]
        _a: Option<IgnoredAny>,
    }
    let header: Header = from_bytes(buf).map_err(|e| Bep51Error::Deserialize(format!("{e}")))?;
    if header.y.as_ref().map(|b| b.as_ref()).unwrap_or(&[]) != b"q" {
        return Ok(None);
    }
    let q = header.q.as_ref().ok_or(Bep51Error::MissingField("q"))?;
    if q.as_ref() != METHOD_NAME {
        return Ok(None);
    }

    // Confirmed sample_infohashes — re-parse with the typed args dict.
    #[derive(Deserialize)]
    struct Full {
        #[serde(rename = "a")]
        a: SampleInfohashesArgs,
    }
    let full: Full = from_bytes(buf).map_err(|e| Bep51Error::Deserialize(format!("{e}")))?;
    Ok(Some((header.t, full.a)))
}

/// Decode a KRPC response envelope into a `SampleInfohashesResp`. Caller is
/// responsible for confirming via the in-flight transaction table that the
/// response actually matches a sample_infohashes query.
pub fn decode_response(
    buf: &[u8],
) -> Result<(ByteBufOwned, SampleInfohashesRespOwned), Bep51Error> {
    #[derive(Deserialize)]
    struct Raw {
        #[serde(rename = "t")]
        t: ByteBufOwned,
        #[serde(rename = "r")]
        r: SampleInfohashesRespOwned,
    }
    let raw: Raw = from_bytes(buf).map_err(|e| Bep51Error::Deserialize(format!("{e}")))?;
    Ok((raw.t, raw.r))
}

/// Iterator yielding each 20-byte sample from the concatenated `samples` blob.
pub fn iter_samples(samples: &[u8]) -> impl Iterator<Item = &[u8]> + '_ {
    samples.chunks_exact(SAMPLE_LEN)
}
