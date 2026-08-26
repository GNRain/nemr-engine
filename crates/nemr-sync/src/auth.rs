//! Bearer tokens and the authenticated-user extractor.
//!
//! A token is opaque random bytes; only its SHA-256 is stored, so a database
//! leak yields nothing usable. Lookup enforces expiry and that the account is
//! active (a `pending_recovery` account cannot act until it confirms recovery).

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::RngCore;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::ApiError;
use crate::AppState;

/// A freshly minted token: the string to hand the client, and the hash to store.
pub struct NewToken {
    pub secret: String,
    pub hash: Vec<u8>,
}

/// Mint a token. 256 bits of entropy from the OS CSPRNG.
pub fn mint_token() -> NewToken {
    let mut raw = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut raw);
    let secret = URL_SAFE_NO_PAD.encode(raw);
    let hash = hash_token(&secret);
    NewToken { secret, hash }
}

/// Hash a presented token for lookup. The stored value is never the token.
pub fn hash_token(secret: &str) -> Vec<u8> {
    let mut h = Sha256::new();
    h.update(secret.as_bytes());
    h.finalize().to_vec()
}

/// An authenticated, active user. Extracting it is the auth gate: any handler
/// that takes it as an argument requires a valid, unexpired token for an active
/// account.
pub struct AuthUser {
    pub id: Uuid,
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let header = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .ok_or(ApiError::Unauthorized)?;
        let secret = header
            .strip_prefix("Bearer ")
            .ok_or(ApiError::Unauthorized)?;
        let hash = hash_token(secret);

        // One query joins the token to its user, enforcing both expiry and
        // active status. A row means: valid token, not expired, account usable.
        let row: Option<(Uuid, String)> = sqlx::query_as(
            "SELECT u.id, u.status
               FROM auth_tokens t
               JOIN users u ON u.id = t.user_id
              WHERE t.token_hash = $1 AND t.expires_at > now()",
        )
        .bind(&hash)
        .fetch_optional(&state.pool)
        .await?;

        match row {
            Some((id, status)) if status == "active" => Ok(AuthUser { id }),
            Some(_) => Err(ApiError::Forbidden(
                "account is pending recovery confirmation".into(),
            )),
            None => Err(ApiError::Unauthorized),
        }
    }
}
