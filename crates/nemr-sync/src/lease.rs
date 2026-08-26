//! The D-03 lease: per session, TTL, heartbeat to renew, explicit takeover.
//!
//! `fence` is a monotonic token bumped on every acquire and takeover, never on a
//! heartbeat. A holder carries `(holder, fence)`. The server honours a heartbeat
//! or a write only while both still match the row and it has not expired, so
//! when another client takes over — advancing `fence` — the loser's next
//! heartbeat and any write it attempts are refused **by the server**, not merely
//! by the loser's own good behaviour. This is the fencing the stateless daemon
//! needs: a silently-restarted daemon whose lease expired must re-acquire, and
//! re-acquire is denied if someone else took over in the gap.

use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::error::{ApiError, ApiResult};
use crate::{index, AppState};

#[derive(Deserialize)]
pub struct AcquireRequest {
    /// Opaque client/machine identity (e.g. a machine name). The lease is held
    /// by whoever presents this and the matching fence.
    pub holder: String,
}

#[derive(Serialize)]
pub struct AcquireResponse {
    /// True if the caller now holds the lease. False means it is held by someone
    /// else (still within TTL); `holder`/`fence` describe the current holder so
    /// the caller can offer an explicit takeover.
    pub granted: bool,
    pub holder: String,
    pub fence: i64,
    pub expires_at_unix: i64,
}

fn expiry(state: &AppState) -> OffsetDateTime {
    OffsetDateTime::now_utc() + state.config.lease_ttl
}

/// Acquire the lease if it is free or already ours. A lease held by another
/// client within its TTL is not taken — that needs [`takeover`].
pub async fn acquire(
    State(state): State<AppState>,
    user: AuthUser,
    Path(name): Path<String>,
    Json(req): Json<AcquireRequest>,
) -> ApiResult<Json<AcquireResponse>> {
    let session_id = index::resolve(&state, user.id, &name).await?;
    let expires = expiry(&state);

    // Atomic: the conflict update fires only if the current lease is expired or
    // already ours. Otherwise no row returns and the lease is held by another.
    let granted: Option<(i64, OffsetDateTime)> = sqlx::query_as(
        "INSERT INTO leases (session_id, holder, fence, expires_at)
         VALUES ($1, $2, 1, $3)
         ON CONFLICT (session_id) DO UPDATE
            SET holder = EXCLUDED.holder,
                fence = leases.fence + 1,
                expires_at = EXCLUDED.expires_at
          WHERE leases.expires_at < now() OR leases.holder = EXCLUDED.holder
         RETURNING fence, expires_at",
    )
    .bind(session_id)
    .bind(&req.holder)
    .bind(expires)
    .fetch_optional(&state.pool)
    .await?;

    if let Some((fence, expires_at)) = granted {
        return Ok(Json(AcquireResponse {
            granted: true,
            holder: req.holder,
            fence,
            expires_at_unix: expires_at.unix_timestamp(),
        }));
    }

    // Held by another. Report who, so the caller can decide whether to take over.
    let (holder, fence, expires_at): (String, i64, OffsetDateTime) =
        sqlx::query_as("SELECT holder, fence, expires_at FROM leases WHERE session_id = $1")
            .bind(session_id)
            .fetch_one(&state.pool)
            .await?;
    Ok(Json(AcquireResponse {
        granted: false,
        holder,
        fence,
        expires_at_unix: expires_at.unix_timestamp(),
    }))
}

#[derive(Deserialize)]
pub struct HeartbeatRequest {
    pub holder: String,
    pub fence: i64,
}

#[derive(Serialize)]
pub struct HeartbeatResponse {
    pub fence: i64,
    pub expires_at_unix: i64,
}

/// Renew the lease. Succeeds only while the caller still holds it (holder and
/// fence match, not expired). A lost lease returns 409 — the caller must stop
/// writing.
pub async fn heartbeat(
    State(state): State<AppState>,
    user: AuthUser,
    Path(name): Path<String>,
    Json(req): Json<HeartbeatRequest>,
) -> ApiResult<Json<HeartbeatResponse>> {
    let session_id = index::resolve(&state, user.id, &name).await?;
    let expires = expiry(&state);

    let renewed: Option<(i64, OffsetDateTime)> = sqlx::query_as(
        "UPDATE leases SET expires_at = $4
          WHERE session_id = $1 AND holder = $2 AND fence = $3 AND expires_at >= now()
         RETURNING fence, expires_at",
    )
    .bind(session_id)
    .bind(&req.holder)
    .bind(req.fence)
    .bind(expires)
    .fetch_optional(&state.pool)
    .await?;

    match renewed {
        Some((fence, expires_at)) => Ok(Json(HeartbeatResponse {
            fence,
            expires_at_unix: expires_at.unix_timestamp(),
        })),
        None => Err(ApiError::Conflict(
            "lease lost: it was taken over or expired; stop writing and re-acquire".into(),
        )),
    }
}

#[derive(Deserialize)]
pub struct TakeoverRequest {
    pub holder: String,
}

/// Force the lease to the caller, advancing the fence so the previous holder's
/// heartbeat and writes are refused. This is the explicit "take over?" path.
pub async fn takeover(
    State(state): State<AppState>,
    user: AuthUser,
    Path(name): Path<String>,
    Json(req): Json<TakeoverRequest>,
) -> ApiResult<Json<AcquireResponse>> {
    let session_id = index::resolve(&state, user.id, &name).await?;
    let expires = expiry(&state);

    let (fence, expires_at): (i64, OffsetDateTime) = sqlx::query_as(
        "INSERT INTO leases (session_id, holder, fence, expires_at)
         VALUES ($1, $2, 1, $3)
         ON CONFLICT (session_id) DO UPDATE
            SET holder = EXCLUDED.holder,
                fence = leases.fence + 1,
                expires_at = EXCLUDED.expires_at
         RETURNING fence, expires_at",
    )
    .bind(session_id)
    .bind(&req.holder)
    .bind(expires)
    .fetch_one(&state.pool)
    .await?;

    Ok(Json(AcquireResponse {
        granted: true,
        holder: req.holder,
        fence,
        expires_at_unix: expires_at.unix_timestamp(),
    }))
}

/// Verify the caller currently holds the lease with the given fence. Used to gate
/// writes: a stale fence (someone took over) or an expired lease is refused, so
/// a losing client cannot write even if it ignores its own heartbeat failure.
pub async fn require_held(
    state: &AppState,
    session_id: Uuid,
    holder: &str,
    fence: i64,
) -> ApiResult<()> {
    let held: Option<(Uuid,)> = sqlx::query_as(
        "SELECT session_id FROM leases
          WHERE session_id = $1 AND holder = $2 AND fence = $3 AND expires_at >= now()",
    )
    .bind(session_id)
    .bind(holder)
    .bind(fence)
    .fetch_optional(&state.pool)
    .await?;
    if held.is_none() {
        return Err(ApiError::Conflict(
            "lease not held: acquire the lease before writing".into(),
        ));
    }
    Ok(())
}
