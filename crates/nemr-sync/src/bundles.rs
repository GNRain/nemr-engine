//! Whole-bundle upload and download through the M12 storage trait.
//!
//! The body is ciphertext (E-16); the server stores and returns it without a key
//! to open it. Upload is gated by the lease: the caller must present the lease
//! holder and fence it holds, so a client that lost the lease cannot write even
//! if it ignores its own heartbeat failure (D-03).

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use nemr_storage::ObjectKey;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::error::{ApiError, ApiResult};
use crate::{index, lease, AppState};

fn storage_key(state: &AppState, user_id: Uuid, session_id: Uuid) -> ApiResult<ObjectKey> {
    let key = format!("{}/{}/{}", state.config.bundle_prefix, user_id, session_id);
    ObjectKey::new(key).map_err(|e| ApiError::Internal(anyhow::anyhow!("bad storage key: {e}")))
}

fn header_str(headers: &HeaderMap, name: &str) -> ApiResult<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .ok_or_else(|| ApiError::BadRequest(format!("missing header {name}")))
}

/// Store a bundle for a session. Requires the lease (headers `x-nemr-lease-holder`
/// and `x-nemr-lease-fence`).
pub async fn upload(
    State(state): State<AppState>,
    user: AuthUser,
    Path(name): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Json<UploadResponse>> {
    let session_id = index::resolve(&state, user.id, &name).await?;

    let holder = header_str(&headers, "x-nemr-lease-holder")?;
    let fence: i64 = header_str(&headers, "x-nemr-lease-fence")?
        .parse()
        .map_err(|_| ApiError::BadRequest("x-nemr-lease-fence must be an integer".into()))?;
    // Fail fast on a fence we can already see is stale, so a doomed upload does
    // not transfer its body first.
    lease::require_held(&state, session_id, &holder, fence).await?;

    let digest = Sha256::digest(&body);
    let key = storage_key(&state, user.id, session_id)?;
    state
        .store
        .put(&key, &body)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("store put: {e}")))?;
    // The operator's read-control for E-20: which key, how many bytes, in
    // which store — never the bytes, never a credential.
    tracing::info!(key = %key.as_str(), bytes = body.len(), store = %state.store.describe(), "stored bundle");

    // F-92: re-check the fence **inside the metadata write itself**. The check
    // above is a separate read, so a takeover landing between it and here would
    // let a fenced-out machine's in-flight upload publish anyway — the exact
    // race D-03's fencing exists to prevent, and one a long body transfer makes
    // wide. The guard is in the WHERE clause, so the update is atomic with the
    // lease test; a stale writer touches no row and is told so.
    let published = sqlx::query(
        "UPDATE sessions
            SET storage_key = $2, ciphertext_sha256 = $3, ciphertext_bytes = $4,
                last_machine = $5, updated_at = now()
          WHERE id = $1
            AND EXISTS (
                SELECT 1 FROM leases l
                 WHERE l.session_id = $1 AND l.holder = $5 AND l.fence = $6
                   AND l.expires_at >= now()
            )",
    )
    .bind(session_id)
    .bind(key.as_str())
    .bind(digest.as_slice())
    .bind(body.len() as i64)
    .bind(&holder)
    .bind(fence)
    .execute(&state.pool)
    .await?
    .rows_affected();

    if published == 0 {
        return Err(ApiError::Conflict(
            "lease lost during the upload: it was taken over or expired mid-transfer; \
             the bundle was not published. Re-acquire the lease and push again."
                .into(),
        ));
    }

    Ok(Json(UploadResponse {
        bytes: body.len() as i64,
        sha256: hex_lower(&digest),
    }))
}

/// E-22: delete a session's cloud copy — the stored object AND the index row —
/// so the merge reads the session as local-only (or drops it entirely if it
/// lived only in the cloud). Refused while an ACTIVE lease is held by ANOTHER
/// machine: deleting a bundle another machine holds would let its next push
/// write into a deleted key and leave a half-state (D-03). The lease test is in
/// the DELETE's own WHERE clause, so a lease acquired mid-delete is refused
/// atomically rather than raced. The object is removed synchronously (not
/// tombstoned): a storage tier bills what the store holds, so what exists is the
/// store's live contents with no tombstone ledger to reconcile.
pub async fn delete_cloud(
    State(state): State<AppState>,
    user: AuthUser,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    // 404 before anything else, so "no such session" is not reported as a lease
    // conflict.
    let session_id = index::resolve(&state, user.id, &name).await?;
    let caller = header_str(&headers, "x-nemr-lease-holder")?;

    let deleted: Option<(Option<String>,)> = sqlx::query_as(
        "DELETE FROM sessions
          WHERE id = $1
            AND NOT EXISTS (
                SELECT 1 FROM leases l
                 WHERE l.session_id = $1 AND l.holder <> $2 AND l.expires_at >= now()
            )
        RETURNING storage_key",
    )
    .bind(session_id)
    .bind(&caller)
    .fetch_optional(&state.pool)
    .await?;

    let Some((storage_key,)) = deleted else {
        // The row exists (resolved above) but was not deleted: an active lease
        // is held by another machine. Name the holder so the client can offer
        // take-over.
        let holder: Option<(String,)> = sqlx::query_as(
            "SELECT holder FROM leases WHERE session_id = $1 AND expires_at >= now()",
        )
        .bind(session_id)
        .fetch_optional(&state.pool)
        .await?;
        let who = holder
            .map(|(h,)| h)
            .unwrap_or_else(|| "another machine".to_string());
        return Err(ApiError::Conflict(format!(
            "the session's lease is held by {who}; take it over before deleting the cloud copy"
        )));
    };

    // The row (and, by cascade, its lease) is gone. Remove the object too, so
    // what the store holds is exactly what exists (E-22). An object-delete
    // failure after the row is gone would orphan bytes — logged loudly rather
    // than silently left.
    if let Some(key) = storage_key {
        let key = ObjectKey::new(key).map_err(|e| ApiError::Internal(anyhow::anyhow!("{e}")))?;
        state.store.delete(&key).await.map_err(|e| {
            tracing::error!(key = %key.as_str(), "deleted the index row but the object delete failed: {e} — possible orphan");
            ApiError::Internal(anyhow::anyhow!("store delete: {e}"))
        })?;
        tracing::info!(key = %key.as_str(), store = %state.store.describe(), "deleted bundle (E-22)");
    }

    Ok(Json(
        serde_json::json!({ "deleted": true, "session": name }),
    ))
}

#[derive(serde::Serialize)]
pub struct UploadResponse {
    pub bytes: i64,
    pub sha256: String,
}

use axum::Json;

/// Return a session's stored bundle (ciphertext). Download does not require the
/// lease — reading is safe for any authenticated owner; only writing is gated.
pub async fn download(
    State(state): State<AppState>,
    user: AuthUser,
    Path(name): Path<String>,
) -> ApiResult<Response> {
    let session_id = index::resolve(&state, user.id, &name).await?;

    let row: Option<(Option<String>,)> =
        sqlx::query_as("SELECT storage_key FROM sessions WHERE id = $1")
            .bind(session_id)
            .fetch_optional(&state.pool)
            .await?;
    let storage_key = row
        .and_then(|(k,)| k)
        .ok_or_else(|| ApiError::NotFound("no bundle stored for this session".into()))?;
    let key =
        ObjectKey::new(storage_key).map_err(|e| ApiError::Internal(anyhow::anyhow!("{e}")))?;

    let bytes = state
        .store
        .get(&key)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("store get: {e}")))?;

    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/octet-stream")],
        bytes,
    )
        .into_response())
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}
