//! The Nemr sync server binary.
//!
//! Configuration from the environment:
//! - `DATABASE_URL` — Postgres connection string (required).
//! - `NEMR_SERVER_ADDR` — listen address (default `127.0.0.1:8080`).
//! - `NEMR_BUNDLE_DIR` — filesystem directory for the storage backend, required
//!   for the local backend; R2/B2 will be a config change.

use std::sync::Arc;

use nemr_sync::{connect_and_migrate, router, AppState, Config, DynStore};
use nemr_storage::local::LocalStore;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "nemr_server=info".into()),
        )
        .init();

    let database_url = std::env::var("DATABASE_URL")
        .map_err(|_| anyhow::anyhow!("DATABASE_URL is required"))?;
    let addr = std::env::var("NEMR_SERVER_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".into());
    let bundle_dir = std::env::var("NEMR_BUNDLE_DIR")
        .map_err(|_| anyhow::anyhow!("NEMR_BUNDLE_DIR is required for the local backend"))?;

    let pool = connect_and_migrate(&database_url).await?;
    let store: Arc<dyn DynStore> = Arc::new(LocalStore::new(bundle_dir));
    let state = AppState {
        pool,
        store,
        config: Config::default(),
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
