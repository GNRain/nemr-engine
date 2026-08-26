//! Registration, recovery confirmation, and login.
//!
//! The server never sees the password or the master key. It receives a
//! client-derived `auth_key` (which it Argon2id-hashes into a verifier), the
//! public KDF salt and parameters, two opaque key envelopes, and the recovery
//! acknowledgement hash. See E-16.

use argon2::{Argon2, Params, PasswordHash, PasswordHasher, PasswordVerifier, Version};
use axum::extract::State;
use axum::Json;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use password_hash::SaltString;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::auth::mint_token;
use crate::error::{ApiError, ApiResult};
use crate::{AppState, KdfCost};

fn b64_decode(s: &str, what: &'static str) -> ApiResult<Vec<u8>> {
    URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|_| ApiError::BadRequest(format!("invalid base64 in {what}")))
}

fn b64_encode(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn normalize_email(email: &str) -> ApiResult<String> {
    let e = email.trim().to_lowercase();
    if e.is_empty() || !e.contains('@') {
        return Err(ApiError::BadRequest("a valid email is required".into()));
    }
    Ok(e)
}

/// Argon2id-hash the client-derived auth key into a PHC verifier string.
fn hash_auth_key(auth_key: &[u8], cost: KdfCost) -> ApiResult<String> {
    let params = Params::new(cost.m_cost, cost.t_cost, cost.p_cost, None)
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("argon2 params: {e}")))?;
    let argon = Argon2::new(argon2::Algorithm::Argon2id, Version::V0x13, params);
    let salt = SaltString::generate(&mut OsRng);
    argon
        .hash_password(auth_key, &salt)
        .map(|h| h.to_string())
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("argon2 hash: {e}")))
}

fn verify_auth_key(auth_key: &[u8], verifier: &str) -> bool {
    match PasswordHash::new(verifier) {
        Ok(parsed) => Argon2::default().verify_password(auth_key, &parsed).is_ok(),
        Err(_) => false,
    }
}

/// Record a login attempt for the rate limiter. Best-effort: a failure to log an
/// attempt must not itself deny an otherwise-valid login.
async fn record_attempt(pool: &sqlx::PgPool, email: &str, succeeded: bool) {
    let _ = sqlx::query("INSERT INTO login_attempts (email, succeeded) VALUES ($1, $2)")
        .bind(email)
        .bind(succeeded)
        .execute(pool)
        .await;
}

// --- Register -------------------------------------------------------------

#[derive(Deserialize)]
pub struct RegisterRequest {
    pub email: String,
    pub kdf_salt: String,
    pub kdf_m_cost: u32,
    pub kdf_t_cost: u32,
    pub kdf_p_cost: u32,
    pub auth_key: String,
    pub password_envelope: String,
    pub recovery_salt: String,
    pub recovery_m_cost: u32,
    pub recovery_t_cost: u32,
    pub recovery_p_cost: u32,
    pub recovery_envelope: String,
    pub recovery_ack_hash: String,
}

#[derive(Serialize)]
pub struct RegisterResponse {
    pub user_id: Uuid,
    /// The account is not usable until recovery is confirmed (E-16). Stated in
    /// the response so a client cannot mistake registration for completion.
    pub status: String,
}

pub async fn register(
    State(state): State<AppState>,
    Json(req): Json<RegisterRequest>,
) -> ApiResult<(axum::http::StatusCode, Json<RegisterResponse>)> {
    let email = normalize_email(&req.email)?;
    let auth_key = b64_decode(&req.auth_key, "auth_key")?;
    let password_envelope = b64_decode(&req.password_envelope, "password_envelope")?;
    let recovery_envelope = b64_decode(&req.recovery_envelope, "recovery_envelope")?;
    let recovery_ack_hash = b64_decode(&req.recovery_ack_hash, "recovery_ack_hash")?;
    let kdf_salt = b64_decode(&req.kdf_salt, "kdf_salt")?;
    let recovery_salt = b64_decode(&req.recovery_salt, "recovery_salt")?;

    // A recovery envelope is required at registration — recovery is not
    // deferrable (E-16). The confirm step still gates usability.
    if recovery_envelope.is_empty() || recovery_ack_hash.len() != 32 {
        return Err(ApiError::BadRequest(
            "a recovery envelope and a 32-byte acknowledgement are required".into(),
        ));
    }

    let verifier = hash_auth_key(&auth_key, state.config.server_kdf)?;

    let row: Result<(Uuid,), sqlx::Error> = sqlx::query_as(
        "INSERT INTO users (email, kdf_salt, kdf_m_cost, kdf_t_cost, kdf_p_cost,
                            auth_verifier, password_envelope,
                            recovery_salt, recovery_m_cost, recovery_t_cost, recovery_p_cost,
                            recovery_envelope, recovery_ack_hash)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)
         RETURNING id",
    )
    .bind(&email)
    .bind(&kdf_salt)
    .bind(req.kdf_m_cost as i32)
    .bind(req.kdf_t_cost as i32)
    .bind(req.kdf_p_cost as i32)
    .bind(&verifier)
    .bind(&password_envelope)
    .bind(&recovery_salt)
    .bind(req.recovery_m_cost as i32)
    .bind(req.recovery_t_cost as i32)
    .bind(req.recovery_p_cost as i32)
    .bind(&recovery_envelope)
    .bind(&recovery_ack_hash)
    .fetch_one(&state.pool)
    .await;

    let user_id = match row {
        Ok((id,)) => id,
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            return Err(ApiError::Conflict("email already registered".into()));
        }
        Err(e) => return Err(e.into()),
    };

    Ok((
        axum::http::StatusCode::CREATED,
        Json(RegisterResponse {
            user_id,
            status: "pending_recovery".into(),
        }),
    ))
}

// --- Confirm recovery -----------------------------------------------------

#[derive(Deserialize)]
pub struct ConfirmRecoveryRequest {
    pub email: String,
    /// `SHA-256(domain || MK)`, recomputed after the client recovered MK through
    /// the recovery envelope with the re-entered code.
    pub recovery_ack_hash: String,
}

pub async fn confirm_recovery(
    State(state): State<AppState>,
    Json(req): Json<ConfirmRecoveryRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let email = normalize_email(&req.email)?;
    let presented = b64_decode(&req.recovery_ack_hash, "recovery_ack_hash")?;

    let row: Option<(Uuid, Vec<u8>, String)> =
        sqlx::query_as("SELECT id, recovery_ack_hash, status FROM users WHERE email = $1")
            .bind(&email)
            .fetch_optional(&state.pool)
            .await?;

    let (id, stored, status) = row.ok_or(ApiError::Unauthorized)?;

    // Constant-time so a probe cannot learn the hash byte by byte.
    if presented.ct_eq(&stored).unwrap_u8() != 1 {
        return Err(ApiError::Unauthorized);
    }

    if status != "active" {
        sqlx::query("UPDATE users SET status = 'active' WHERE id = $1")
            .bind(id)
            .execute(&state.pool)
            .await?;
    }

    Ok(Json(serde_json::json!({ "status": "active" })))
}

// --- KDF params (pre-login) -----------------------------------------------

#[derive(Deserialize)]
pub struct KdfParamsRequest {
    pub email: String,
}

#[derive(Serialize)]
pub struct KdfParamsResponse {
    pub kdf_salt: String,
    pub kdf_m_cost: u32,
    pub kdf_t_cost: u32,
    pub kdf_p_cost: u32,
}

/// Return the KDF salt and parameters for an email so a fresh machine can derive
/// its auth key before it can log in.
///
/// Enumeration resistance is a documented gap for this pass (F-89): an unknown
/// email currently 404s, which distinguishes it from a known one. The hardening
/// is a deterministic pseudo-salt from a server pepper, tracked separately.
pub async fn kdf_params(
    State(state): State<AppState>,
    Json(req): Json<KdfParamsRequest>,
) -> ApiResult<Json<KdfParamsResponse>> {
    let email = normalize_email(&req.email)?;
    let row: Option<(Vec<u8>, i32, i32, i32)> = sqlx::query_as(
        "SELECT kdf_salt, kdf_m_cost, kdf_t_cost, kdf_p_cost FROM users WHERE email = $1",
    )
    .bind(&email)
    .fetch_optional(&state.pool)
    .await?;
    let (salt, m, t, p) = row.ok_or_else(|| ApiError::NotFound("no such account".into()))?;
    Ok(Json(KdfParamsResponse {
        kdf_salt: b64_encode(&salt),
        kdf_m_cost: m as u32,
        kdf_t_cost: t as u32,
        kdf_p_cost: p as u32,
    }))
}

// --- Login ----------------------------------------------------------------

#[derive(Deserialize)]
pub struct LoginRequest {
    pub email: String,
    pub auth_key: String,
}

#[derive(Serialize)]
pub struct LoginResponse {
    pub token: String,
    /// The password envelope and KDF material, so the client can derive its wrap
    /// key and unwrap the master key locally. All opaque to the server.
    pub password_envelope: String,
    pub kdf_salt: String,
    pub kdf_m_cost: u32,
    pub kdf_t_cost: u32,
    pub kdf_p_cost: u32,
}

pub async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> ApiResult<Json<LoginResponse>> {
    let email = normalize_email(&req.email)?;
    let auth_key = b64_decode(&req.auth_key, "auth_key")?;

    // Rate limit before doing any expensive work or revealing anything.
    let cutoff = OffsetDateTime::now_utc() - state.config.login_window;
    let failures: (i64,) = sqlx::query_as(
        "SELECT count(*) FROM login_attempts
          WHERE email = $1 AND succeeded = false AND attempted_at > $2",
    )
    .bind(&email)
    .bind(cutoff)
    .fetch_one(&state.pool)
    .await?;
    if failures.0 >= state.config.max_login_failures {
        return Err(ApiError::TooManyRequests);
    }

    let row: Option<LoginRow> = sqlx::query_as(
        "SELECT id, status, auth_verifier, password_envelope, kdf_salt, kdf_m_cost, kdf_t_cost, kdf_p_cost
           FROM users WHERE email = $1",
    )
    .bind(&email)
    .fetch_optional(&state.pool)
    .await?;

    let Some(u) = row else {
        record_attempt(&state.pool, &email, false).await;
        return Err(ApiError::Unauthorized);
    };

    if !verify_auth_key(&auth_key, &u.auth_verifier) {
        record_attempt(&state.pool, &email, false).await;
        return Err(ApiError::Unauthorized);
    }

    if u.status != "active" {
        // Correct credentials but recovery not yet confirmed. Not a failure to
        // rate-limit, but not a login either.
        return Err(ApiError::Forbidden(
            "account is pending recovery confirmation".into(),
        ));
    }

    let token = mint_token();
    let expires = OffsetDateTime::now_utc() + state.config.token_ttl;
    sqlx::query("INSERT INTO auth_tokens (token_hash, user_id, expires_at) VALUES ($1, $2, $3)")
        .bind(&token.hash)
        .bind(u.id)
        .bind(expires)
        .execute(&state.pool)
        .await?;
    record_attempt(&state.pool, &email, true).await;

    Ok(Json(LoginResponse {
        token: token.secret,
        password_envelope: b64_encode(&u.password_envelope),
        kdf_salt: b64_encode(&u.kdf_salt),
        kdf_m_cost: u.kdf_m_cost as u32,
        kdf_t_cost: u.kdf_t_cost as u32,
        kdf_p_cost: u.kdf_p_cost as u32,
    }))
}

#[derive(sqlx::FromRow)]
struct LoginRow {
    id: Uuid,
    status: String,
    auth_verifier: String,
    password_envelope: Vec<u8>,
    kdf_salt: Vec<u8>,
    kdf_m_cost: i32,
    kdf_t_cost: i32,
    kdf_p_cost: i32,
}
