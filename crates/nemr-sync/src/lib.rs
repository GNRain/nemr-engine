//! The Nemr sync server (WP-J): identity, session index, bundle storage, lease.
//!
//! COMMERCIAL side of E-11. It holds only what it can act on without reading a
//! user's data: an Argon2id auth verifier, opaque key envelopes, unencrypted
//! index metadata, and the lease. Bundles are ciphertext (E-16); the server
//! stores and returns them without a key to open them.
//!
//! The public surface is [`router`], which takes an [`AppState`] so tests can
//! run the whole API in-process against a real Postgres.

use std::sync::Arc;

use axum::routing::{get, post, put};
use axum::Router;
use sqlx::postgres::PgPool;
use time::Duration;

mod auth;
mod bundles;
mod db;
mod error;
mod identity;
mod index;
mod lease;
mod store;

pub use db::connect_and_migrate;
pub use store::DynStore;

/// Server configuration. Defaults are production-sane; tests lower the KDF cost
/// and shorten TTLs.
#[derive(Clone)]
pub struct Config {
    /// How long a bearer token stays valid.
    pub token_ttl: Duration,
    /// The lease TTL: a holder must heartbeat within this or lose the lease.
    pub lease_ttl: Duration,
    /// Server-side Argon2id cost for the auth verifier. The client's Argon2id is
    /// the brute-force barrier; this only keeps the stored verifier from being
    /// the transmitted value.
    pub server_kdf: KdfCost,
    /// Rate limiting: at most this many failed logins per email within
    /// [`Config::login_window`] before further attempts are refused.
    pub max_login_failures: i64,
    pub login_window: Duration,
    /// Key prefix under which bundles are stored in the object store.
    pub bundle_prefix: String,
}

/// Argon2id cost, mirrored from `nemr_crypto::KdfParams` but named here so the
/// server carries no dependency on the crypto crate.
#[derive(Clone, Copy)]
pub struct KdfCost {
    pub m_cost: u32,
    pub t_cost: u32,
    pub p_cost: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            token_ttl: Duration::days(30),
            lease_ttl: Duration::seconds(60),
            // OWASP Argon2id: 19 MiB, t=2, p=1.
            server_kdf: KdfCost {
                m_cost: 19 * 1024,
                t_cost: 2,
                p_cost: 1,
            },
            max_login_failures: 5,
            login_window: Duration::minutes(15),
            bundle_prefix: "bundles".to_string(),
        }
    }
}

/// Everything a handler needs. Cheap to clone (a pool handle, an `Arc`, and
/// small config).
#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub store: Arc<dyn DynStore>,
    pub config: Config,
}

/// Build the full API router. Mounting is centralised here so the route table is
/// readable in one place.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/v1/register", post(identity::register))
        .route("/v1/recovery/confirm", post(identity::confirm_recovery))
        .route("/v1/auth/params", post(identity::kdf_params))
        .route("/v1/login", post(identity::login))
        .route("/v1/sessions", get(index::list).post(index::upsert))
        .route(
            "/v1/sessions/{name}/bundle",
            put(bundles::upload).get(bundles::download),
        )
        .route("/v1/sessions/{name}/lease", post(lease::acquire))
        .route(
            "/v1/sessions/{name}/lease/heartbeat",
            post(lease::heartbeat),
        )
        .route("/v1/sessions/{name}/lease/takeover", post(lease::takeover))
        .with_state(state)
}
