//! The session index: unencrypted metadata so the list works on any machine.
//!
//! This is the only thing about a session the server can read (E-16). The bundle
//! itself is ciphertext behind [`crate::bundles`].

use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::error::ApiResult;
use crate::AppState;

#[derive(Serialize)]
pub struct SessionEntry {
    pub id: Uuid,
    pub name: String,
    pub agent: String,
    pub size_bytes: i64,
    pub description: String,
    pub base_image_version: String,
    pub last_machine: Option<String>,
    pub has_bundle: bool,
    pub ciphertext_bytes: Option<i64>,
    pub updated_at_unix: i64,
    /// Who holds the D-03 lease right now, if anyone — "open on <machine>" in
    /// the list, the state the lease UX was designed around. Only a lease that
    /// has not expired counts; an expired row is nobody.
    pub held_by: Option<String>,
    pub lease_expires_at_unix: Option<i64>,
}

/// List the caller's sessions, most-recently-updated first.
pub async fn list(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<SessionEntry>>> {
    let rows: Vec<SessionRow> = sqlx::query_as(
        "SELECT s.id, s.name, s.agent, s.size_bytes, s.description, s.base_image_version,
                s.last_machine, s.storage_key, s.ciphertext_bytes, s.updated_at,
                l.holder AS held_by, l.expires_at AS lease_expires_at
           FROM sessions s
           LEFT JOIN leases l ON l.session_id = s.id AND l.expires_at >= now()
          WHERE s.user_id = $1
          ORDER BY s.updated_at DESC",
    )
    .bind(user.id)
    .fetch_all(&state.pool)
    .await?;

    let entries = rows
        .into_iter()
        .map(|r| SessionEntry {
            id: r.id,
            name: r.name,
            agent: r.agent,
            size_bytes: r.size_bytes,
            description: r.description,
            base_image_version: r.base_image_version,
            last_machine: r.last_machine,
            has_bundle: r.storage_key.is_some(),
            ciphertext_bytes: r.ciphertext_bytes,
            updated_at_unix: r.updated_at.unix_timestamp(),
            held_by: r.held_by,
            lease_expires_at_unix: r.lease_expires_at.map(|t| t.unix_timestamp()),
        })
        .collect();
    Ok(Json(entries))
}

#[derive(sqlx::FromRow)]
struct SessionRow {
    id: Uuid,
    name: String,
    agent: String,
    size_bytes: i64,
    description: String,
    base_image_version: String,
    last_machine: Option<String>,
    storage_key: Option<String>,
    ciphertext_bytes: Option<i64>,
    updated_at: OffsetDateTime,
    held_by: Option<String>,
    lease_expires_at: Option<OffsetDateTime>,
}

#[derive(Deserialize)]
pub struct UpsertRequest {
    pub name: String,
    pub agent: String,
    #[serde(default)]
    pub size_bytes: i64,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub base_image_version: String,
    #[serde(default)]
    pub last_machine: Option<String>,
}

#[derive(Serialize)]
pub struct UpsertResponse {
    pub id: Uuid,
}

/// Create or update the index entry for a session by name. Idempotent per
/// (user, name); metadata fields are overwritten, the stored bundle is left
/// untouched.
pub async fn upsert(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<UpsertRequest>,
) -> ApiResult<Json<UpsertResponse>> {
    let (id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO sessions (user_id, name, agent, size_bytes, description,
                               base_image_version, last_machine, updated_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7, now())
         ON CONFLICT (user_id, name) DO UPDATE
            SET agent = EXCLUDED.agent,
                size_bytes = EXCLUDED.size_bytes,
                description = EXCLUDED.description,
                base_image_version = EXCLUDED.base_image_version,
                last_machine = EXCLUDED.last_machine,
                updated_at = now()
         RETURNING id",
    )
    .bind(user.id)
    .bind(&req.name)
    .bind(&req.agent)
    .bind(req.size_bytes)
    .bind(&req.description)
    .bind(&req.base_image_version)
    .bind(&req.last_machine)
    .fetch_one(&state.pool)
    .await?;
    Ok(Json(UpsertResponse { id }))
}

/// Resolve a session id from the caller's name, or 404. Shared by the lease and
/// bundle handlers.
pub async fn resolve(state: &AppState, user_id: Uuid, name: &str) -> ApiResult<Uuid> {
    let row: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM sessions WHERE user_id = $1 AND name = $2")
            .bind(user_id)
            .bind(name)
            .fetch_optional(&state.pool)
            .await?;
    row.map(|(id,)| id)
        .ok_or_else(|| crate::error::ApiError::NotFound("no such session".into()))
}
