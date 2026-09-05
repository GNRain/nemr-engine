//! Claude Code credential injection (Section 3.5, AUTH-01..AUTH-03).
//!
//! Credentials are never baked into the base image (AUTH-01). The credential
//! file — and only that file — is bind-mounted from the host at container
//! creation, read-write since D-02's (f) so Claude Code can refresh the login
//! inside the session (AUTH-02), and a missing host credential is a clear,
//! actionable failure rather than a container that starts and then cannot
//! authenticate (AUTH-03).

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
/// 2. share the host's own `history.jsonl` and caches with every session, in
///    both directions, since the mount must be writable for the refresh.
///
/// Mounting only `.credentials.json` satisfies what AUTH-02 is for — the
/// container authenticates with the host's login and can renew it — without
/// either consequence. The write surface a session gains is that one file.
/// This narrowing is recorded in SPEC.md Section 11; the read-write change
/// under D-02's (f), with the enumeration of `~/.claude`, in DECISIONS.md.
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
             The host's credential file is mounted into every session (AUTH-02); \
             a session refreshes it but cannot create it.",
            path.display()
        );
    }

    if !path.is_file() {
        bail!(
            "the credential path exists but is not a regular file.\n\
             path:     {}\n\
             expected: the Claude Code credentials file\n\
             found:    {}\n\n\
             It is bind-mounted into the container (AUTH-02), and only a \
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

/// The facts a credential file states about itself, read without judging them.
///
/// Under D-02's (f) the credential is mounted read-write, so Claude Code
/// refreshes the **access token** inside the session from the **refresh
/// token** — routine, every eight hours, not a fault. What can no longer be
/// recovered from inside a session is a spent refresh token, or a file Claude
/// Code has *blanked* after a dead refresh (its "dead-token disk clear" writes
/// empty tokens and `expiresAt: 0`). Those two, and only those, need the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CredentialFacts {
    /// `claudeAiOauth.expiresAt` — the access token.
    pub access: CredentialExpiry,
    /// `claudeAiOauth.refreshTokenExpiresAt` — the refresh token.
    pub refresh: CredentialExpiry,
    /// The OAuth object is present but its `accessToken` is empty: the shape
    /// Claude Code leaves behind after a dead-token clear.
    pub blank: bool,
}

/// What the facts mean right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialVerdict {
    /// No OAuth shape to judge: an API key, a placeholder, or unparseable.
    NotOauth,
    /// The access token is live.
    Fresh { access_left: i64 },
    /// The access token is spent and the refresh token is live (or undated):
    /// Claude Code refreshes on next use. Not a warning.
    Refreshable { refresh_left: Option<i64> },
    /// The refresh token is spent: nothing in the file can recover it. Log in
    /// on the host.
    RefreshExpired { since: i64 },
    /// Claude Code cleared the tokens after a dead refresh. Log in on the host.
    Blank,
}

impl CredentialFacts {
    pub fn verdict(self, now_unix: i64) -> CredentialVerdict {
        if self.blank {
            return CredentialVerdict::Blank;
        }
        match self.access {
            CredentialExpiry::Unknown => CredentialVerdict::NotOauth,
            CredentialExpiry::At(access) if access > now_unix => CredentialVerdict::Fresh {
                access_left: access - now_unix,
            },
            CredentialExpiry::At(_) => match self.refresh {
                CredentialExpiry::At(refresh) if refresh <= now_unix => {
                    CredentialVerdict::RefreshExpired {
                        since: now_unix - refresh,
                    }
                }
                CredentialExpiry::At(refresh) => CredentialVerdict::Refreshable {
                    refresh_left: Some(refresh - now_unix),
                },
                // An older file shape with no dated refresh token: Claude Code
                // can still try; we have no evidence it cannot, so no warning.
                CredentialExpiry::Unknown => CredentialVerdict::Refreshable { refresh_left: None },
            },
        }
    }
}

/// Parse the facts out of a credential file's contents. Total, like
/// [`credential_expiry`]: anything unparseable is "no facts", never an error.
pub fn credential_facts(contents: &str) -> CredentialFacts {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(contents) else {
        return CredentialFacts {
            access: CredentialExpiry::Unknown,
            refresh: CredentialExpiry::Unknown,
            blank: false,
        };
    };
    let oauth = value.get("claudeAiOauth");
    let secs = |field: &str| {
        oauth
            .and_then(|o| o.get(field))
            .and_then(serde_json::Value::as_i64)
            .map(|millis| CredentialExpiry::At(millis / 1000))
            .unwrap_or(CredentialExpiry::Unknown)
    };
    CredentialFacts {
        access: secs("expiresAt"),
        refresh: secs("refreshTokenExpiresAt"),
        blank: oauth.is_some()
            && oauth
                .and_then(|o| o.get("accessToken"))
                .and_then(|t| t.as_str())
                .is_none_or(str::is_empty),
    }
}

/// Read and parse the facts of the credential file at `path`.
pub fn credential_facts_at(path: &Path) -> CredentialFacts {
    match std::fs::read_to_string(path) {
        Ok(contents) => credential_facts(&contents),
        Err(_) => credential_facts(""),
    }
}

/// Does a running session see a *different* file than the host has now (F-12)?
///
/// A file bind mount pins the inode it was made from. Claude Code on the host
/// rewrites the credential by rename on every refresh, so after a host-side
/// refresh the session's mount still shows the previous file — whose refresh
/// token has been rotated away — and the session's next refresh will fail and
/// blank its own copy. Read, never repaired: the daemon's own user can stat the
/// task's view through `/proc/<pid>/root`, so this compares device and inode
/// of what the session sees at `container_path` with `host_path`. `None` when
/// either side cannot be read.
pub fn credential_is_stale(task_pid: u32, container_path: &Path, host_path: &Path) -> Option<bool> {
    use std::os::unix::fs::MetadataExt;
    let seen = std::fs::metadata(
        std::path::Path::new("/proc")
            .join(task_pid.to_string())
            .join("root")
            .join(container_path.strip_prefix("/").unwrap_or(container_path)),
    )
    .ok()?;
    let host = std::fs::metadata(host_path).ok()?;
    Some((seen.dev(), seen.ino()) != (host.dev(), host.ino()))
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

    /// The five verdicts, from the file shapes Claude Code actually writes
    /// (measured 2026-09-05: access token 8 h, refresh token ~2 weeks; the
    /// dead-token clear leaves `accessToken: ""` and `expiresAt: 0`).
    #[test]
    fn credential_verdicts_follow_the_two_tokens() {
        let now: i64 = 1_800_000_000;
        let file = |access: i64, refresh: i64, token: &str| {
            format!(
                r#"{{"claudeAiOauth":{{"accessToken":"{token}","refreshToken":"r","expiresAt":{},"refreshTokenExpiresAt":{}}}}}"#,
                access * 1000,
                refresh * 1000
            )
        };
        assert_eq!(
            credential_facts(&file(now + 3600, now + 86_400, "a")).verdict(now),
            CredentialVerdict::Fresh { access_left: 3600 }
        );
        assert_eq!(
            credential_facts(&file(now - 3600, now + 86_400, "a")).verdict(now),
            CredentialVerdict::Refreshable {
                refresh_left: Some(86_400)
            },
            "a spent access token with a live refresh token is routine, not a fault"
        );
        assert_eq!(
            credential_facts(&file(now - 3600, now - 60, "a")).verdict(now),
            CredentialVerdict::RefreshExpired { since: 60 }
        );
        assert_eq!(
            credential_facts(&file(0, now + 86_400, "")).verdict(now),
            CredentialVerdict::Blank,
            "the dead-token clear must read as Blank, not as a refreshable expiry"
        );
        // No dated refresh token: Claude Code may still refresh; no warning.
        assert_eq!(
            credential_facts(r#"{"claudeAiOauth":{"accessToken":"a","expiresAt":1000}}"#)
                .verdict(now),
            CredentialVerdict::Refreshable { refresh_left: None }
        );
        assert_eq!(
            credential_facts(r#"{"_comment":"CI PLACEHOLDER"}"#).verdict(now),
            CredentialVerdict::NotOauth
        );
        assert_eq!(
            credential_facts("").verdict(now),
            CredentialVerdict::NotOauth
        );
    }

    /// The F-12 detector compares what a task sees with what the host has, by
    /// device and inode through /proc/<pid>/root. Our own pid sees the host's
    /// filesystem, so the same path must read as not stale and a different file
    /// as stale; an unreadable side is "cannot tell", never a verdict.
    #[test]
    fn credential_staleness_is_an_inode_comparison() {
        let dir = std::env::temp_dir().join(format!("nemr-auth-stale-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.json");
        let b = dir.join("b.json");
        std::fs::write(&a, "{}").unwrap();
        std::fs::write(&b, "{}").unwrap();
        let me = std::process::id();
        assert_eq!(
            credential_is_stale(me, &a, &a),
            Some(false),
            "same file: not stale"
        );
        assert_eq!(
            credential_is_stale(me, &a, &b),
            Some(true),
            "a different inode: stale"
        );
        // Replace-by-rename, exactly what the host's Claude Code does on refresh.
        std::fs::rename(&b, &a).unwrap();
        assert_eq!(
            credential_is_stale(me, &a, &a),
            Some(false),
            "after the rename both sides read the new inode"
        );
        assert_eq!(credential_is_stale(me, &dir.join("missing"), &a), None);
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
