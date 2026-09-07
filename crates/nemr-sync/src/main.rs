//! The Nemr sync server binary.
//!
//! Configuration comes from the process environment and from one file the
//! server reads itself, `~/.config/nemr/sync.env` (see `settings`):
//! - `DATABASE_URL` — Postgres connection string (required).
//! - `NEMR_SERVER_ADDR` — listen address (default `127.0.0.1:8080`).
//! - `NEMR_AUTH_PEPPER` — required (E-19): the server refuses to bind
//!   without one; `ephemeral` is the loud escape hatch for a throwaway server.
//! - the storage backend (E-20): exactly one of `NEMR_BUNDLE_DIR` (an
//!   existing directory) and the `NEMR_S3_*` set (an object store).
//! - `NEMR_BUNDLE_PREFIX` — key prefix in the store (default `bundles`).
//!
//! Start-up order is the ruling's: settings, then the store opened and
//! probed, then the database migrated, then the port bound — a server that
//! is listening is one whose store answered.

use nemr_sync::settings::{self, Pepper, Settings};
use nemr_sync::{connect_and_migrate, router, store, AppState, Config};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Colour only on a terminal: a journal or a captured log file gets plain
    // text, so an operator's grep (and the acceptance's) reads the line the
    // server wrote, not its escape codes.
    use std::io::IsTerminal as _;
    tracing_subscriber::fmt()
        .with_ansi(std::io::stdout().is_terminal())
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "nemr_sync=info".into()),
        )
        .init();

    let env = |k: &str| std::env::var(k).ok();
    let settings = Settings::load(&env)?;
    // Names only, never values: the file holds the pepper and a credential.
    match &settings.file {
        Some(path) if path.exists() => tracing::info!(
            file = %path.display(),
            keys = %settings.from_file.join(","),
            "settings: read from file (environment wins key by key)"
        ),
        Some(path) => {
            tracing::info!(file = %path.display(), "settings: no file (environment only)")
        }
        None => tracing::info!("settings: NEMR_SYNC_ENV_FILE is empty, environment only"),
    }
    let get = |k: &str| settings.get(k, &env);

    let database_url = get("DATABASE_URL").ok_or_else(|| {
        anyhow::anyhow!(
            "DATABASE_URL is required (environment or {})",
            settings.file_for_messages()
        )
    })?;
    let addr = get("NEMR_SERVER_ADDR").unwrap_or_else(|| "127.0.0.1:8080".into());

    let mut config = Config::default();
    if let Some(prefix) = get("NEMR_BUNDLE_PREFIX") {
        config.bundle_prefix = prefix;
    }
    match settings::pepper(&settings, &env)? {
        Pepper::Configured(p) => config.auth_pepper = p,
        Pepper::Ephemeral => tracing::warn!(
            "NEMR_AUTH_PEPPER=ephemeral — THROWAWAY SERVER: the pepper is random for this process, so \
             account-enumeration resistance at /v1/auth/params resets on restart (F-89). Never for real accounts."
        ),
    }

    // The store first (E-20): chosen from its own variables, opened, and
    // probed before anything else has a side effect.
    let choice = settings::select_storage(&get)?;
    let store = store::open_store(&choice, &get)?;
    tracing::info!(store = %store.describe(), "storage backend");
    store::preflight(store.as_ref(), &config.bundle_prefix).await?;
    tracing::info!(store = %store.describe(), "storage backend reachable");

    let pool = connect_and_migrate(&database_url).await?;

    let state = AppState {
        pool,
        store,
        config,
    };

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!(%addr, "nemr sync server listening");
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutting down");
}
