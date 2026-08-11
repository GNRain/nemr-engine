//! Claude Code credential injection (Section 3.5, AUTH-01..AUTH-03).
//!
//! Credentials are never baked into the base image (AUTH-01). They are
//! bind-mounted read-only from the host at container creation (AUTH-02), and a
//! missing host credential is a clear, actionable failure rather than a
//! container that starts and then cannot authenticate (AUTH-03).

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

/// Host credential file, relative to the user's home directory.
const HOST_CREDENTIALS_RELATIVE: &str = ".claude/.credentials.json";

/// Locate the host's Claude Code credentials.
///
/// # Why a single file rather than the whole `~/.claude` directory
///
/// AUTH-02 says "credentials directory". `~/.claude` is not only credentials:
/// it also holds `projects/`, `history.jsonl`, caches and plugin state.
/// Mounting all of it into every project container would
///
/// 1. expose every other project's history to any code running in any
///    container, which defeats the isolation the product exists to provide,
///    and
/// 2. break Claude Code anyway, because AUTH-02 requires the mount be
///    read-only and Claude Code writes session state under `~/.claude`.
///
/// Mounting only `.credentials.json` satisfies what AUTH-02 is for — the
/// container authenticates using host credentials it cannot modify — without
/// either consequence. This narrowing is recorded in SPEC.md Section 11.
pub fn host_credentials_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set; cannot locate credentials")?;
    Ok(PathBuf::from(home).join(HOST_CREDENTIALS_RELATIVE))
}

/// Resolve the host credential file, failing per AUTH-03 if absent.
///
/// The error names the file and says what to do, because the actionable fix
/// (authenticate on the host) is not something the engine can perform:
/// interactive in-container authentication is out of scope for Phase 1.
pub fn resolve_credentials() -> Result<PathBuf> {
    let path = host_credentials_path()?;

    if !path.exists() {
        bail!(
            "no Claude Code credentials found at {}.\n\
             Authenticate on the host first by running `claude` and completing login, \
             then retry.\n\
             Credentials are mounted from the host read-only (AUTH-02); in-container \
             authentication is out of scope for Phase 1.",
            path.display()
        );
    }

    if !path.is_file() {
        bail!(
            "{} exists but is not a regular file; expected the Claude Code credentials file",
            path.display()
        );
    }

    Ok(path)
}

/// Warn if the credential file is more permissive than owner-only.
///
/// Not fatal — it is the user's file and their call — but R-05 flags
/// credential-mount permission implications as worth reviewing, and a
/// world-readable credential is worth saying out loud once.
pub fn check_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mode = std::fs::metadata(path)
        .with_context(|| format!("cannot stat {}", path.display()))?
        .permissions()
        .mode()
        & 0o777;

    if mode & 0o077 != 0 {
        eprintln!(
            "[aihub:auth] warning: {} is mode {mode:04o}; credentials are readable beyond \
             the owner. Consider `chmod 600 {}`.",
            path.display(),
            path.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_path_is_under_home() {
        let path = host_credentials_path().unwrap();
        assert!(path.ends_with(".claude/.credentials.json"), "got {path:?}");
        assert!(path.is_absolute());
    }

    #[test]
    fn missing_credentials_error_is_actionable() {
        // Point HOME at a directory with no credentials and check the message
        // tells the user what to do, not merely that something is absent.
        let temp = std::env::temp_dir().join("aihub-auth-test-empty");
        std::fs::create_dir_all(&temp).unwrap();

        let previous = std::env::var_os("HOME");
        std::env::set_var("HOME", &temp);
        let result = resolve_credentials();
        match previous {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }

        let error = result.unwrap_err().to_string();
        assert!(error.contains(".credentials.json"), "should name the file: {error}");
        assert!(error.contains("Authenticate on the host"), "should say what to do: {error}");
    }
}
