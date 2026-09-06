//! The UI process as a gRPC client of the daemon (E-11 ruling, 2026-09-06):
//! what the UI needs from the engine, asked over the daemon's Unix socket
//! through `nemr-daemon-api` — never by linking the engine, never by
//! scraping the CLI.
//!
//! `engine_cli` remains the CLI's path (a subprocess the user could run by
//! hand); this is the UI's, because a long-lived server process holds a
//! connection and speaks the daemon's own protocol. The sync core sees
//! either through `EngineOps`; the UI's own verbs (start, stop) are here too.

use std::path::Path;

use anyhow::{Context, Result};
use nemr_daemon_api::proto::{
    ExportRequest, ImportRequest, ListRequest, StartRequest, StopRequest,
};

use crate::core::EngineOps;
use crate::engine_cli::LocalProject;

/// The daemon, driven from blocking code (the sync core is blocking; the
/// UI's jobs run on blocking threads) on the UI's own runtime.
pub struct DaemonEngine {
    rt: tokio::runtime::Handle,
}

impl DaemonEngine {
    pub fn new(rt: tokio::runtime::Handle) -> Self {
        Self { rt }
    }

    /// One connected session per call: autostart, version handshake, audit
    /// stream — the same path every CLI command takes.
    async fn session() -> Result<nemr_daemon_api::client::Session> {
        nemr_daemon_api::client::connect()
            .await
            .context("reaching the daemon")
    }

    /// The local projects, from the daemon's `List` — the same rows `nemr
    /// list` shows, in the shape the CLI's subprocess path produces.
    pub async fn list_projects() -> Result<Vec<LocalProject>> {
        let mut s = Self::session().await?;
        let req = s.req(ListRequest {});
        let reply = s
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

    pub async fn import_bundle(bundle: &Path, name: &str) -> Result<String> {
        let mut s = Self::session().await?;
        let req = s.req(ImportRequest {
            bundle_path: bundle.to_string_lossy().into_owned(),
            name: name.to_string(),
            size: String::new(),
        });
        let reply = s
            .client()
            .import(req)
            .await
            .map_err(|st| anyhow::anyhow!("{}", st.message()))
            .context("importing the bundle")?
            .into_inner();
        Ok(reply.name)
    }

    pub async fn export_bundle(name: &str, dest: &Path) -> Result<()> {
        let mut s = Self::session().await?;
        let req = s.req(ExportRequest {
            name: name.to_string(),
            destination: dest.to_string_lossy().into_owned(),
            include_build_artifacts: false,
        });
        s.client()
            .export(req)
            .await
            .map_err(|st| anyhow::anyhow!("{}", st.message()))
            .context("exporting the session")?;
        Ok(())
    }

    pub async fn start_project(name: &str) -> Result<()> {
        let mut s = Self::session().await?;
        let req = s.req(StartRequest {
            name: name.to_string(),
        });
        s.client()
            .start(req)
            .await
            .map_err(|st| anyhow::anyhow!("{}", st.message()))
            .context("starting the session")?;
        Ok(())
    }

    /// Stop; returns the daemon's outcome word (graceful, killed, no_task,
    /// wedged).
    pub async fn stop_project(name: &str) -> Result<String> {
        let mut s = Self::session().await?;
        let req = s.req(StopRequest {
            name: name.to_string(),
        });
        let reply = s
            .client()
            .stop(req)
            .await
            .map_err(|st| anyhow::anyhow!("{}", st.message()))
            .context("stopping the session")?
            .into_inner();
        Ok(reply.outcome)
    }
}

impl EngineOps for DaemonEngine {
    fn list(&self) -> Result<Vec<LocalProject>> {
        self.rt.block_on(Self::list_projects())
    }
    fn export(&self, name: &str, dest: &Path) -> Result<()> {
        self.rt.block_on(Self::export_bundle(name, dest))
    }
    fn import(&self, bundle: &Path, name: &str) -> Result<String> {
        self.rt.block_on(Self::import_bundle(bundle, name))
    }
}

impl crate::serve::UiEngine for DaemonEngine {
    fn start(&self, name: &str) -> Result<()> {
        self.rt.block_on(Self::start_project(name))
    }
    fn stop(&self, name: &str) -> Result<String> {
        self.rt.block_on(Self::stop_project(name))
    }
}
