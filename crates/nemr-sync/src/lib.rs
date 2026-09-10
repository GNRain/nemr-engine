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

use axum::extract::DefaultBodyLimit;
use axum::routing::{delete, get, post, put};
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
pub mod settings;
pub mod store;

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
    /// Secret used to derive a deterministic pseudo-salt for an unknown email at
    /// the KDF-params endpoint, so it cannot be used to enumerate accounts
    /// (F-89). It must be secret: if an observer knew it, they could recompute
    /// the pseudo-salt and tell it apart from a real account's stored salt. The
    /// default is random per process; set `NEMR_AUTH_PEPPER` for a value stable
    /// across restarts.
    pub auth_pepper: [u8; 32],
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
            auth_pepper: random_pepper(),
        }
    }
}

/// A random 32-byte pepper from the OS CSPRNG. Used as the default so a server
/// started with no `NEMR_AUTH_PEPPER` is still enumeration-resistant within a
/// process; `main` prefers the env value for stability across restarts.
pub fn random_pepper() -> [u8; 32] {
    use rand::RngCore;
    let mut p = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut p);
    p
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
        .route("/v1/logout", post(auth::logout))
        .route("/v1/sessions", get(index::list).post(index::upsert))
        // F-16: a bundle is the product's payload and can be gigabytes (the
        // quota presets go to 10GB). axum's 2MB default body limit silently
        // capped every push — a 6MB session answered 413, and anything larger
        // had the connection closed mid-stream, which reaches the client as a
        // broken pipe. The limit is lifted HERE and replaced by the upload
        // handler's own ceiling, which it enforces while streaming to disk
        // rather than by buffering the body in memory.
        .route(
            "/v1/sessions/{name}/bundle",
            // The disable applies to THIS method only: chaining `.layer` on the
            // method router scopes it to the routes added before it, so the
            // download and every other endpoint keep axum's default.
            put(bundles::upload)
                .layer(DefaultBodyLimit::disable())
                .get(bundles::download),
        )
        // E-22: delete the cloud copy — the object AND the index row. Refused
        // while another machine holds the lease (take over first).
        .route("/v1/sessions/{name}", delete(bundles::delete_cloud))
        .route("/v1/sessions/{name}/lease", post(lease::acquire))
        .route(
            "/v1/sessions/{name}/lease/heartbeat",
            post(lease::heartbeat),
        )
        .route("/v1/sessions/{name}/lease/takeover", post(lease::takeover))
        .route("/v1/sessions/{name}/lease/release", post(lease::release))
        .with_state(state)
}
