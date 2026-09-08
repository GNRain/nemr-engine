//! Driving the open engine's CLI as a subprocess.
//!
//! The client deliberately does not link the engine: `nemr` is the engine's
//! public surface, and everything the client does through it a user could do by
//! hand — which keeps the commercial half honest about where the open half
//! ends. `NEMR_BIN` overrides the binary (the test suite points it at a stub).

use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

fn nemr_bin() -> String {
    std::env::var("NEMR_BIN").unwrap_or_else(|_| "nemr".into())
}

fn run(args: &[&str]) -> Result<String> {
    let bin = nemr_bin();
    let out = Command::new(&bin).args(args).output().with_context(|| {
        format!(
            "running `{bin} {}` — is the engine installed?",
            args.join(" ")
        )
    })?;
    if !out.status.success() {
        bail!(
            "`{bin} {}` failed:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// A local project row from `nemr list --json`. Only what the client reads;
/// serde skips the rest, so the engine may grow the JSON freely.
#[derive(Deserialize, Clone)]
pub struct LocalProject {
    pub name: String,
    pub agent: String,
    pub running: bool,
    pub usage_known: bool,
    pub used_bytes: u64,
    /// E-21: whether this machine has a Claude login for its sessions (the
    /// daemon's `credential_present`). `None` when the source cannot say —
    /// the CLI's `nemr list --json` does not carry it.
    #[serde(default)]
    pub credential_present: Option<bool>,
}

#[derive(Deserialize)]
struct ListJson {
    projects: Vec<LocalProject>,
}

/// The local project list, from `nemr list --json` — the stable machine
/// surface, so this never scrapes the human table.
pub fn list_projects() -> Result<Vec<LocalProject>> {
    let out = run(&["list", "--json"])?;
    let parsed: ListJson = serde_json::from_str(&out).context("parsing `nemr list --json`")?;
    Ok(parsed.projects)
}

/// Export a project to `dest` (absolute: the daemon resolves relative paths in
/// its own cwd, not ours).
pub fn export(name: &str, dest: &Path) -> Result<()> {
    let dest = dest.to_string_lossy();
    run(&["export", name, "-o", &dest]).map(|_| ())
}

/// Import a bundle; the engine creates the project, taking name and quota from
/// the manifest. An existing name is refused by the engine with its own remedy.
pub fn import(bundle: &Path) -> Result<String> {
    let bundle = bundle.to_string_lossy();
    run(&["import", &bundle])
}
