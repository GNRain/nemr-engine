//! The UI process as a gRPC client of the daemon (E-11 ruling, 2026-09-06):
//! what the UI needs from the engine, asked over the daemon's Unix socket
//! through `nemr-daemon-api` — never by linking the engine, never by
//! scraping the CLI.
//!
//! `engine_cli` remains the CLI's path (a subprocess the user could run by
//! hand); this is the UI's, because a long-lived server process holds a
//! connection and speaks the daemon's own protocol.

use anyhow::{Context, Result};
use nemr_daemon_api::proto::ListRequest;

use crate::engine_cli::LocalProject;

/// The local projects, from the daemon's `List` — the same rows `nemr list`
/// shows, in the same shape the CLI's subprocess path produces.
pub async fn list_projects() -> Result<Vec<LocalProject>> {
    let mut session = nemr_daemon_api::client::connect()
        .await
        .context("reaching the daemon for the project list")?;
    let req = session.req(ListRequest {});
    let reply = session
        .client()
        .list(req)
        .await
        .context("the daemon's List call")?
        .into_inner();
    Ok(reply
        .projects
        .into_iter()
        .map(|p| LocalProject {
            name: p.name,
            agent: p.agent,
            running: p.running,
            usage_known: p.usage_known,
            used_bytes: p.used_bytes,
        })
        .collect())
}
