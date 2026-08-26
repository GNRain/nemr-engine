//! Database pool and migrations.

use sqlx::postgres::{PgPool, PgPoolOptions};

/// Connect and run migrations. The migrations are embedded at compile time from
/// `migrations/`, so this needs no database reachable when the crate is built —
/// only when it runs.
///
/// Pool size is `NEMR_DB_MAX_CONN` (default 16). The integration suite lowers it
/// so many concurrently-spawned test servers do not exhaust a small Postgres.
pub async fn connect_and_migrate(database_url: &str) -> anyhow::Result<PgPool> {
    let max = std::env::var("NEMR_DB_MAX_CONN")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(16);
    let pool = PgPoolOptions::new()
        .max_connections(max)
        .connect(database_url)
        .await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    Ok(pool)
}
