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
            "the credential path exists but is not a regular file.\n\
             path:     {}\n\
             expected: the Claude Code credentials file\n\
             found:    {}\n\n\
             It is bind-mounted read-only into the container (AUTH-02), and only a \
             regular file can be. Remove or rename whatever is there, then \
             authenticate on the host by running `claude`.",
            path.display(),
            if path.is_dir() {
                "a directory"
            } else if path.is_symlink() {
                "a symlink"
            } else {
                "something that is neither a regular file nor a directory"
            }
        );
    }

    Ok(path)
}

/// The expiry state of a Claude Code credential, read without judging it.
///
/// The file Claude Code writes is `{"claudeAiOauth": {"expiresAt": <ms>, ...}}`
/// for a subscription login. An API-key credential, the CI placeholder, and any
/// shape this build does not recognise have no such field — and the absence of
/// an expiry is *not* an expired credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialExpiry {
    /// An OAuth `expiresAt` was present. Unix **seconds** (converted from the
    /// file's milliseconds), so it composes with `SystemTime`/`unix` maths.
    At(i64),
    /// The file was read but carried no `claudeAiOauth.expiresAt`: an API-key
    /// credential, a placeholder, or an unrecognised shape. Reported as its own
    /// state rather than folded into "expired", because a check that cried wolf
    /// on every non-OAuth credential is the F-56 shape this exists to avoid.
    Unknown,
}

impl CredentialExpiry {
    /// Is this credential expired relative to `now_unix` (seconds)?
    ///
    /// `Unknown` is **never** expired: we have no evidence that it is, and the
    /// whole point of this type is to warn *only* when we can stand behind it.
    /// The boundary is inclusive — a token whose `expiresAt` is exactly now is
    /// spent.
    pub fn is_expired_at(self, now_unix: i64) -> bool {
        matches!(self, CredentialExpiry::At(secs) if secs <= now_unix)
    }

    /// The expiry in unix seconds, if one was present. `None` for `Unknown`.
    pub fn expires_at_secs(self) -> Option<i64> {
        match self {
            CredentialExpiry::At(secs) => Some(secs),
            CredentialExpiry::Unknown => None,
        }
    }
}

/// Parse the OAuth expiry out of a credential file's contents.
///
/// Pure and **total**: every input yields a value, never an error. A credential
/// we cannot parse is `Unknown`, not a failure — a parser that could fail would
/// itself become a new way for `status` and `attach` to break, which is exactly
/// the opposite of what reading the expiry is for.
pub fn credential_expiry(contents: &str) -> CredentialExpiry {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(contents) else {
        return CredentialExpiry::Unknown;
    };
    match value
        .get("claudeAiOauth")
        .and_then(|oauth| oauth.get("expiresAt"))
        .and_then(serde_json::Value::as_i64)
    {
        // The file stores milliseconds since the epoch; the rest of the engine
        // works in seconds. Convert here, once, so no caller re-derives it.
        Some(millis) => CredentialExpiry::At(millis / 1000),
        None => CredentialExpiry::Unknown,
    }
}

/// Read and parse the expiry of the credential file at `path`.
///
/// A file that cannot be read is `Unknown`, for the same reason a file that
/// cannot be parsed is: this is a *diagnostic* read, and it must not turn into a
/// failure of the command that called it.
pub fn credential_expiry_at(path: &Path) -> CredentialExpiry {
    match std::fs::read_to_string(path) {
        Ok(contents) => credential_expiry(&contents),
        Err(_) => CredentialExpiry::Unknown,
    }
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
            "[nemr:auth] warning: {} is mode {mode:04o}; credentials are readable beyond \
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

    /// The file stores milliseconds; the engine reports seconds. A wrong unit
    /// here would make every credential "expire" ~50,000 years out, or report
    /// a live one as spent since 1970 — both silently, both wrong, so the
    /// conversion is pinned to a real value from a real file.
    #[test]
    fn credential_expiry_reads_the_oauth_expiry_in_seconds() {
        let json = r#"{"claudeAiOauth":{"accessToken":"x","refreshToken":"y","expiresAt":1788479173829,"scopes":["user:inference"]}}"#;
        assert_eq!(
            credential_expiry(json),
            CredentialExpiry::At(1788479173),
            "1788479173829 ms must read as 1788479173 s"
        );
    }

    /// No `claudeAiOauth.expiresAt` means Unknown — never Expired. This is the
    /// CI placeholder's exact shape, and an API-key credential's; a check that
    /// reported either as expired would fire on every CI run and every API-key
    /// user, and a warning that always fires is a warning nobody reads.
    #[test]
    fn credential_expiry_without_the_field_is_unknown_not_expired() {
        let placeholder = r#"{"_comment": "CI PLACEHOLDER — not a credential. Exercises the AUTH-02 mount path only; the API round-trip is skipped in CI (NEMR_SKIP_API=1)."}"#;
        assert_eq!(credential_expiry(placeholder), CredentialExpiry::Unknown);
        assert_eq!(
            credential_expiry("not json at all"),
            CredentialExpiry::Unknown
        );
        assert_eq!(credential_expiry(""), CredentialExpiry::Unknown);
        assert!(
            !CredentialExpiry::Unknown.is_expired_at(i64::MAX),
            "Unknown must never read as expired, however late the clock"
        );
    }

    /// The boundary: expired at the instant of expiry, not one second later.
    #[test]
    fn credential_expiry_boundary_is_inclusive() {
        let e = CredentialExpiry::At(1_000);
        assert!(
            !e.is_expired_at(999),
            "one second before expiry: still valid"
        );
        assert!(e.is_expired_at(1_000), "at expiry: spent");
        assert!(e.is_expired_at(1_001), "after expiry: spent");
        assert_eq!(e.expires_at_secs(), Some(1_000));
        assert_eq!(CredentialExpiry::Unknown.expires_at_secs(), None);
    }

    /// The gate `nemr attach` warns on, proven against real files: an expired
    /// credential fires, a valid one stays silent, a placeholder stays silent.
    /// This is the Finding-2 property end to end minus the `eprintln`.
    #[test]
    fn attach_warning_gate_fires_on_expired_and_is_silent_otherwise() {
        let dir = std::env::temp_dir().join(format!("nemr-auth-expiry-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let now: i64 = 1_800_000_000;

        let expired = dir.join("expired.json");
        std::fs::write(
            &expired,
            format!(
                r#"{{"claudeAiOauth":{{"expiresAt":{}}}}}"#,
                (now - 2 * 86_400) * 1000
            ),
        )
        .unwrap();
        assert!(
            credential_expiry_at(&expired).is_expired_at(now),
            "a credential that expired two days ago must fire the warning"
        );

        let valid = dir.join("valid.json");
        std::fs::write(
            &valid,
            format!(
                r#"{{"claudeAiOauth":{{"expiresAt":{}}}}}"#,
                (now + 6 * 3_600) * 1000
            ),
        )
        .unwrap();
        assert!(
            !credential_expiry_at(&valid).is_expired_at(now),
            "a credential valid for six more hours must stay silent"
        );

        let placeholder = dir.join("placeholder.json");
        std::fs::write(&placeholder, r#"{"_comment":"CI PLACEHOLDER"}"#).unwrap();
        assert!(
            !credential_expiry_at(&placeholder).is_expired_at(now),
            "a placeholder has no expiry and must stay silent"
        );

        assert!(
            !credential_expiry_at(&dir.join("missing.json")).is_expired_at(now),
            "an unreadable file is Unknown, not expired"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_credentials_error_is_actionable() {
        // Point HOME at a directory with no credentials and check the message
        // tells the user what to do, not merely that something is absent.
        let temp = std::env::temp_dir().join("nemr-auth-test-empty");
        std::fs::create_dir_all(&temp).unwrap();

        let previous = std::env::var_os("HOME");
        std::env::set_var("HOME", &temp);
        let result = resolve_credentials();
        match previous {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }

        let error = result.unwrap_err().to_string();
        assert!(
            error.contains(".credentials.json"),
            "should name the file: {error}"
        );
        assert!(
            error.contains("Authenticate on the host"),
            "should say what to do: {error}"
        );
    }
}
