/// Signed peer-record store — DHT-style address announcements for rsSync.
///
/// PUT /api/v1/peer-records/{device_id}
///   Body: { addresses, expiry, public_key, signature }
///   Verifies the Ed25519 signature over canonical bytes before storing.
///   The first writer for a device_id establishes the public key (TOFU);
///   subsequent writes must use the same public key.
///
/// GET /api/v1/peer-records/{device_id}
///   Returns the record if it exists and has not expired.
use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Json;
use axum::routing::put;
use serde::{Deserialize, Serialize};

use indexarr_identity::verify_signature;

use crate::state::AppState;

/// Body for PUT /api/v1/peer-records/{device_id}.
#[derive(Debug, Deserialize)]
pub struct PutRecordRequest {
    /// List of addresses, e.g. ["quic://1.2.3.4:22000", "tcp://1.2.3.4:22000"]
    pub addresses: Vec<String>,
    /// Unix timestamp (seconds) when this record should be considered stale.
    pub expiry: i64,
    /// Base64-encoded Ed25519 verifying key (32 bytes).
    pub public_key: String,
    /// Base64-encoded Ed25519 signature (64 bytes) over canonical bytes.
    pub signature: String,
}

/// Response body for GET /api/v1/peer-records/{device_id}.
#[derive(Debug, Serialize)]
pub struct GetRecordResponse {
    pub device_id: String,
    pub addresses: Vec<String>,
    pub expiry: i64,
    pub public_key: String,
    pub updated_at: String,
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route(
        "/api/v1/peer-records/{device_id}",
        put(put_record).get(get_record),
    )
}

/// Store or refresh a signed peer record.
async fn put_record(
    State(state): State<Arc<AppState>>,
    Path(device_id): Path<String>,
    Json(body): Json<PutRecordRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Basic input validation.
    if device_id.len() != 64 || !device_id.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err((
            StatusCode::BAD_REQUEST,
            "device_id must be 64 hex chars".into(),
        ));
    }
    if body.addresses.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "addresses must not be empty".into(),
        ));
    }
    if body.expiry <= 0 {
        return Err((StatusCode::BAD_REQUEST, "expiry must be positive".into()));
    }

    // Build canonical signed bytes: "{device_id}\n{addresses_json_sorted}\n{expiry}"
    let mut sorted_addrs = body.addresses.clone();
    sorted_addrs.sort();
    let addrs_json = serde_json::to_string(&sorted_addrs)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let canonical = format!("{}\n{}\n{}", device_id, addrs_json, body.expiry);

    // Verify signature.
    if !verify_signature(&body.public_key, &body.signature, canonical.as_bytes()) {
        return Err((
            StatusCode::UNAUTHORIZED,
            "signature verification failed".into(),
        ));
    }

    // Check for conflicting public key (TOFU).
    let existing_key: Option<String> =
        sqlx::query_scalar("SELECT public_key FROM peer_records WHERE device_id = $1")
            .bind(&device_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if let Some(stored_key) = existing_key
        && stored_key != body.public_key
    {
        return Err((
            StatusCode::CONFLICT,
            "public_key does not match stored key for this device_id".into(),
        ));
    }

    let expiry_dt = chrono::DateTime::from_timestamp(body.expiry, 0)
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "invalid expiry timestamp".into()))?;

    sqlx::query(
        "INSERT INTO peer_records (device_id, addresses, expiry, public_key, signature, updated_at)
         VALUES ($1, $2, $3, $4, $5, NOW())
         ON CONFLICT (device_id) DO UPDATE
           SET addresses  = EXCLUDED.addresses,
               expiry     = EXCLUDED.expiry,
               signature  = EXCLUDED.signature,
               updated_at = NOW()",
    )
    .bind(&device_id)
    .bind(serde_json::json!(&body.addresses))
    .bind(expiry_dt)
    .bind(&body.public_key)
    .bind(&body.signature)
    .execute(&state.pool)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Retrieve a peer record by device ID.
///
/// Returns 404 if not found or expired.
async fn get_record(
    State(state): State<Arc<AppState>>,
    Path(device_id): Path<String>,
) -> Result<Json<GetRecordResponse>, (StatusCode, String)> {
    if device_id.len() != 64 || !device_id.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err((
            StatusCode::BAD_REQUEST,
            "device_id must be 64 hex chars".into(),
        ));
    }

    type Row = (
        String,
        serde_json::Value,
        chrono::DateTime<chrono::Utc>,
        String,
        chrono::DateTime<chrono::Utc>,
    );
    let row: Option<Row> = sqlx::query_as(
        "SELECT device_id, addresses, expiry, public_key, updated_at
         FROM peer_records
         WHERE device_id = $1 AND expiry > NOW()",
    )
    .bind(&device_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let (did, addrs_val, expiry_dt, public_key, updated_at) =
        row.ok_or((StatusCode::NOT_FOUND, "record not found or expired".into()))?;

    let addresses: Vec<String> = serde_json::from_value(addrs_val)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(GetRecordResponse {
        device_id: did,
        addresses,
        expiry: expiry_dt.timestamp(),
        public_key,
        updated_at: updated_at.to_rfc3339(),
    }))
}
