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

/// The dedicated host directory that holds nemr's Claude credential, relative
/// to the user's home. It holds ONLY `.credentials.json` and the `projects`
/// and `sessions` mount points the volume binds land on. Since F-14 this whole
/// directory is bound over `/root/.claude` in every session (a directory bind,
/// not a single-file one): Claude Code writes the credential by writing a temp
/// file and renaming it over the target — measured 2026-09-08,
/// `.credentials.json.tmp.<hex>` then `rename` — which changes the inode, and a
/// single-file bind cannot follow a rename, so a login's later write escaped
/// onto a container-only inode while the host kept an earlier one (the human-arm
/// failure E-21's file bind hit). It is deliberately NOT the host's own
/// `~/.claude`, which holds history, sessions and settings that must never
/// enter a session (D-02): a dedicated directory is the one whose entire
/// contents may be exposed.
const HOST_CREDENTIAL_DIR_RELATIVE: &str = ".local/share/nemr/host-credential";
const CREDENTIALS_FILE_NAME: &str = ".credentials.json";

/// The host directory that predates F-14 (`~/.claude`), where a login used to
/// live. An existing login there is inherited once into the dedicated
/// directory (a host-local copy; the credential never travels — D-02), so a
/// machine that logged in before F-14 keeps its session login.
fn legacy_host_credentials_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".claude").join(CREDENTIALS_FILE_NAME))
}

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
    // E-21's test seam, as ruled: TEST-ONLY, PATH-ONLY, and settable by
    // nothing but the daemon's own environment — the page cannot reach a
    // process environment and the sync server never talks to the daemon. It
    // exists because "a host that has never logged in" cannot otherwise be
    // produced on a developer host (the NEMR_TEST_PRE_F14 shape). It changes
    // where the credential file is looked for, and nothing else.
    if let Some(explicit) = std::env::var_os("NEMR_HOST_CREDENTIALS") {
        if !explicit.is_empty() {
            return Ok(PathBuf::from(explicit));
        }
    }
    let home = std::env::var_os("HOME").context("HOME is not set; cannot locate credentials")?;
    Ok(PathBuf::from(home)
        .join(HOST_CREDENTIAL_DIR_RELATIVE)
        .join(CREDENTIALS_FILE_NAME))
}

/// The dedicated host directory bound over `/root/.claude` (F-14): the parent
/// of the credential file. Under the test seam it is the seam path's parent,
/// which is why the seam names a file inside a directory of its own.
pub fn host_credential_dir() -> Result<PathBuf> {
    Ok(host_credentials_path()?
        .parent()
        .context("the credential path has no parent directory")?
        .to_path_buf())
}

/// Whether the path is being overridden by the test seam.
fn under_seam() -> bool {
    std::env::var_os("NEMR_HOST_CREDENTIALS")
        .filter(|v| !v.is_empty())
        .is_some()
}

/// The placeholder the engine writes where a host has no credential yet
/// (E-21, ruled 2026-09-08): a regular 0600 file in a shape this build
/// recognises as "no login yet" — never as expired, never as blank — so a
/// session can be created and started on a machine that has never logged
/// in, and Claude Code's own `/login` inside it writes the real credential
/// through the read-write bind onto this very file (D-02 (f)). Nothing in
/// it is a secret; nothing in it authenticates.
///
/// F-10 (measured on the fresh VM, 2026-09-08): Claude Code rewrites this
/// file as a JSON object and keeps unknown top-level keys, so a marker at
/// the top level survived a real `/login`. The marker therefore lives INSIDE
/// `claudeAiOauth`, the object a login replaces, with no token fields beside
/// it — measured: Claude Code answers "Not logged in · Please run /login"
/// and leaves the file alone, where a blank token beside the marker reads as
/// an expired session. And "still the placeholder" is never the marker's
/// presence: it is the absence of a real token (`is_placeholder`).
pub const PLACEHOLDER: &str = r#"{"claudeAiOauth":{"_nemr_placeholder":"no Claude login on this machine yet — attach a session and run /login; the login stays on this machine (D-02, E-21, F-10)"}}"#;

const MARKER: &str = "_nemr_placeholder";

/// The marker, at the top level or inside `claudeAiOauth`.
fn has_marker(v: &serde_json::Value) -> bool {
    v.get(MARKER).is_some()
        || v.get("claudeAiOauth")
            .is_some_and(|o| o.get(MARKER).is_some())
}

/// A real token: `claudeAiOauth.accessToken` is a non-empty string. The
/// one fact "logged in" rests on (F-10); the marker is not a state.
fn has_real_token(v: &serde_json::Value) -> bool {
    v.get("claudeAiOauth")
        .and_then(|o| o.get("accessToken"))
        .and_then(serde_json::Value::as_str)
        .is_some_and(|t| !t.is_empty())
}

/// Is this credential file's content the engine's placeholder — the marker
/// with no real token beside it? A real token beside a leftover marker is a
/// login (F-10).
pub fn is_placeholder(contents: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(contents)
        .ok()
        .is_some_and(|v| has_marker(&v) && !has_real_token(&v))
}

/// F-10: clear a leftover marker on first detection of a real token, so the
/// file is what Claude Code alone would have written. Textual removal of the
/// marker member, so every other byte stays as Claude Code wrote it; checked
/// against the parsed value, with a re-serialisation as the fallback if the
/// surgery ever disagrees. In place — the file is bind-mounted into running
/// sessions and must keep its inode — and mode untouched. Returns whether
/// anything was cleared; a placeholder, a blank clear, a clean login and an
/// unparseable file are all left exactly alone.
pub fn scrub_placeholder_marker(path: &std::path::Path) -> Result<bool> {
    use std::io::Write;
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Ok(false);
    };
    if !(has_marker(&value) && has_real_token(&value)) {
        return Ok(false);
    }
    let mut want = value.clone();
    if let Some(o) = want.as_object_mut() {
        o.remove(MARKER);
        if let Some(inner) = o.get_mut("claudeAiOauth").and_then(|i| i.as_object_mut()) {
            inner.remove(MARKER);
        }
    }
    let mut cleared = text.clone();
    while let Some(next) = remove_string_member(&cleared, MARKER) {
        cleared = next;
    }
    let surgery_ok = serde_json::from_str::<serde_json::Value>(&cleared).is_ok_and(|v| v == want);
    let cleared = if surgery_ok {
        cleared
    } else {
        serde_json::to_string(&want).context("re-serialising the credential without the marker")?
    };
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)
        .with_context(|| format!("opening {} to clear the placeholder marker", path.display()))?;
    f.write_all(cleared.as_bytes())
        .with_context(|| format!("clearing the placeholder marker in {}", path.display()))?;
    Ok(true)
}

/// Remove one `"key": "string"` member from JSON text, with the comma that
/// joined it to its neighbour. `None` when the key is not there as a member
/// with a string value.
fn remove_string_member(text: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let start = text.find(&needle)?;
    let bytes = text.as_bytes();
    // After the key: optional whitespace, ':', optional whitespace, a string.
    let mut i = start + needle.len();
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if bytes.get(i) != Some(&b':') {
        return None;
    }
    i += 1;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if bytes.get(i) != Some(&b'"') {
        return None;
    }
    i += 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'"' => break,
            _ => i += 1,
        }
    }
    if bytes.get(i) != Some(&b'"') {
        return None;
    }
    let mut end = i + 1;
    // A following comma joins this member to the next; else the comma before.
    let mut j = end;
    while j < bytes.len() && bytes[j].is_ascii_whitespace() {
        j += 1;
    }
    let mut begin = start;
    if bytes.get(j) == Some(&b',') {
        end = j + 1;
        while end < bytes.len() && bytes[end].is_ascii_whitespace() {
            end += 1;
        }
    } else {
        let mut k = start;
        while k > 0 && bytes[k - 1].is_ascii_whitespace() {
            k -= 1;
        }
        if k > 0 && bytes[k - 1] == b',' {
            begin = k - 1;
        }
    }
    Some(format!("{}{}", &text[..begin], &text[end..]))
}

/// The host credential path, with the placeholder written there when the
/// file does not exist (E-21). `create` and `start` both call this, so a
/// machine that has never logged in behaves the same whichever way a
/// project arrived — the AUTH-03 amendment. A file that exists is left
/// exactly as it is.
pub fn ensure_host_credential_file() -> Result<PathBuf> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let path = host_credentials_path()?;
    let dir = path
        .parent()
        .context("the credential path has no parent directory")?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    // The directory is bound whole into the session (F-14) and is the user's:
    // private, 0700.
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    // The mount points the volume's session-state binds land on once this
    // directory is bound over `/root/.claude`. Empty directories on the host;
    // their contents come from the volume, per session (M8).
    for sub in ["projects", "sessions"] {
        let m = dir.join(sub);
        std::fs::create_dir_all(&m).with_context(|| format!("creating {}", m.display()))?;
    }
    if path.exists() {
        return Ok(path);
    }
    // AUTH-02, preserved across F-14's relocation: a machine that logged in
    // before F-14 has its credential at `~/.claude/.credentials.json`. Inherit
    // it once, here, by a host-local copy — the credential never leaves the
    // machine, so D-02 holds. Never under the test seam (which must produce a
    // machine with no login), and never a placeholder.
    if !under_seam() {
        if let Some(legacy) = legacy_host_credentials_path() {
            if legacy != path {
                if let Ok(text) = std::fs::read_to_string(&legacy) {
                    if !is_placeholder(&text) {
                        let mut f = std::fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .mode(0o600)
                            .open(&path)
                            .with_context(|| {
                                format!("inheriting the login into {}", path.display())
                            })?;
                        f.write_all(text.as_bytes()).with_context(|| {
                            format!("writing the inherited login to {}", path.display())
                        })?;
                        return Ok(path);
                    }
                }
            }
        }
    }
    // create_new: never truncate a file that appeared between the check and
    // the write (a login landing at that moment).
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
    {
        Ok(mut f) => {
            f.write_all(PLACEHOLDER.as_bytes())
                .with_context(|| format!("writing the placeholder to {}", path.display()))?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e).with_context(|| format!("creating {}", path.display())),
    }
    Ok(path)
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
    /// The engine's own placeholder (E-21): no login on this machine yet.
    pub placeholder: bool,
}

/// What the facts mean right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialVerdict {
    /// The engine's placeholder (E-21): this machine has never logged in.
    /// Not dead, not expired — a session starts and `/login` inside it is
    /// the remedy.
    NoLoginYet,
    /// No OAuth shape to judge: an API key, a CI placeholder, or unparseable.
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
        if self.placeholder {
            return CredentialVerdict::NoLoginYet;
        }
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
            placeholder: false,
        };
    };
    // F-10: the marker is not a state; no real token beside it is.
    let placeholder = has_marker(&value) && !has_real_token(&value);
    let oauth = value.get("claudeAiOauth");
    let secs = |field: &str| {
        oauth
            .and_then(|o| o.get(field))
            .and_then(serde_json::Value::as_i64)
            .map(|millis| CredentialExpiry::At(millis / 1000))
            .unwrap_or(CredentialExpiry::Unknown)
    };
    CredentialFacts {
        placeholder,
        access: secs("expiresAt"),
        refresh: secs("refreshTokenExpiresAt"),
        blank: !placeholder
            && oauth.is_some()
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

/// Refuse to provision against a credential that cannot authenticate
/// (AUTH-03, extended 2026-09-06): a spent refresh token or a file Claude Code
/// has blanked after a dead refresh. A dead credential is worse than a missing
/// one — it looks present — and there is no point provisioning a session that
/// will fail at its first request. Only those two states refuse: an expired
/// access token with a live refresh token is routine (the session refreshes
/// it), and a non-OAuth file (an API key, the CI placeholder) is not judged.
pub fn refuse_dead_credential(path: &Path, now_unix: i64) -> Result<()> {
    let verdict = credential_facts_at(path).verdict(now_unix);
    let what = match verdict {
        CredentialVerdict::Blank => "is BLANK: Claude Code cleared it after a refresh was refused (the login was revoked, or its refresh token spent)",
        CredentialVerdict::RefreshExpired { .. } => "has an EXPIRED refresh token: nothing in it can authenticate any more",
        _ => return Ok(()),
    };
    bail!(
        "the Claude Code credential on this host {what}.\n\
         path: {}\n\
         A session created now would fail at its first request, and a login from inside \
         a session cannot repair the host's login. Run `claude` on this host and log in, \
         then create the project.",
        path.display()
    )
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
    fn credentials_path_is_the_dedicated_dir_under_home() {
        let path = host_credentials_path().unwrap();
        // F-14: a dedicated directory, not the host's own ~/.claude.
        assert!(
            path.ends_with(".local/share/nemr/host-credential/.credentials.json"),
            "got {path:?}"
        );
        assert!(path.is_absolute());
        assert_eq!(host_credential_dir().unwrap(), path.parent().unwrap());
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

    /// The create-time refusal fires on exactly the two dead states and on
    /// nothing else — a routine access expiry and a non-OAuth file pass.
    #[test]
    fn create_refuses_a_dead_credential_and_nothing_else() {
        let dir = std::env::temp_dir().join(format!("nemr-auth-dead-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let now: i64 = 1_800_000_000;
        let write = |name: &str, body: String| {
            let p = dir.join(name);
            std::fs::write(&p, body).unwrap();
            p
        };
        let blank = write("blank.json", r#"{"claudeAiOauth":{"accessToken":"","refreshToken":"","expiresAt":0,"refreshTokenExpiresAt":1900000000000}}"#.into());
        let err = refuse_dead_credential(&blank, now).unwrap_err().to_string();
        assert!(err.contains("BLANK") && err.contains("log in"), "{err}");
        let spent = write(
            "spent.json",
            format!(
                r#"{{"claudeAiOauth":{{"accessToken":"a","refreshToken":"r","expiresAt":{},"refreshTokenExpiresAt":{}}}}}"#,
                (now - 3600) * 1000,
                (now - 60) * 1000
            ),
        );
        let err = refuse_dead_credential(&spent, now).unwrap_err().to_string();
        assert!(err.contains("EXPIRED refresh token"), "{err}");
        // Controls: these must NOT refuse.
        let routine = write(
            "routine.json",
            format!(
                r#"{{"claudeAiOauth":{{"accessToken":"a","refreshToken":"r","expiresAt":{},"refreshTokenExpiresAt":{}}}}}"#,
                (now - 3600) * 1000,
                (now + 86_400) * 1000
            ),
        );
        assert!(
            refuse_dead_credential(&routine, now).is_ok(),
            "an expired access token with a live refresh token is routine"
        );
        let fresh = write(
            "fresh.json",
            format!(
                r#"{{"claudeAiOauth":{{"accessToken":"a","refreshToken":"r","expiresAt":{}}}}}"#,
                (now + 3600) * 1000
            ),
        );
        assert!(refuse_dead_credential(&fresh, now).is_ok());
        let placeholder = write(
            "placeholder.json",
            r#"{"_comment":"CI PLACEHOLDER"}"#.into(),
        );
        assert!(
            refuse_dead_credential(&placeholder, now).is_ok(),
            "a non-OAuth file is not judged"
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

#[cfg(test)]
mod f10_tests {
    use super::*;

    /// F-10 (measured on the fresh VM, 2026-09-08): Claude Code rewrites the
    /// credential file as a JSON object and keeps unknown top-level keys, so
    /// the old marker survived a real `/login` and the engine kept saying
    /// "no login yet". Present means the OAuth fields; the marker is not a
    /// state. The placeholder now keeps its marker INSIDE `claudeAiOauth`
    /// with no token fields — measured: Claude Code answers "Not logged in ·
    /// Please run /login" and leaves the file alone, where a blank token
    /// beside the marker reads as an expired session.
    #[test]
    fn f10_a_marker_beside_a_real_token_is_a_login_not_the_placeholder() {
        let now = 1_800_000_000;
        // The fresh VM's file after /login: the real object, the old marker kept.
        let survived = r#"{"_nemr_placeholder":"no Claude login on this machine yet","claudeAiOauth":{"accessToken":"sk-ant-oat01-x","refreshToken":"sk-ant-ort01-y","expiresAt":1800003600000,"scopes":["user:inference"],"subscriptionType":"max","refreshTokenExpiresAt":1802000000000}}"#;
        assert!(
            !is_placeholder(survived),
            "a real token beside the marker is a login"
        );
        assert!(matches!(
            credential_facts(survived).verdict(now),
            CredentialVerdict::Fresh { .. }
        ));
        // The marker beside real fields inside the object is a login too.
        let nested = r#"{"claudeAiOauth":{"_nemr_placeholder":"x","accessToken":"sk-ant-oat01-x","refreshToken":"y","expiresAt":1800003600000}}"#;
        assert!(!is_placeholder(nested));
        // The placeholder itself is exactly no-login-yet; the clear is still blank.
        assert!(is_placeholder(PLACEHOLDER));
        assert_eq!(
            credential_facts(PLACEHOLDER).verdict(now),
            CredentialVerdict::NoLoginYet
        );
        let blank = r#"{"claudeAiOauth":{"accessToken":"","refreshToken":"","expiresAt":0}}"#;
        assert_eq!(
            credential_facts(blank).verdict(now),
            CredentialVerdict::Blank
        );
        // Its shape: the marker inside claudeAiOauth, no token fields, nothing at the top level.
        let v: serde_json::Value = serde_json::from_str(PLACEHOLDER).unwrap();
        assert!(v["claudeAiOauth"]["_nemr_placeholder"].is_string());
        assert!(v["claudeAiOauth"].get("accessToken").is_none());
        assert!(v.get("_nemr_placeholder").is_none());
    }

    /// The leftover marker is cleared on first detection of a real token, so
    /// the file is byte for byte what Claude Code alone would have written;
    /// in place (the file is bind-mounted into running sessions: same inode),
    /// mode kept. Nothing else is ever touched: not the placeholder, not a
    /// blank clear, not garbage, not a clean login.
    #[test]
    fn f10_the_leftover_marker_is_cleared_once_a_real_token_is_seen() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let dir = std::env::temp_dir().join(format!("nemr-f10-unit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".credentials.json");
        let put = |text: &str| {
            std::fs::write(&path, text).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            std::fs::metadata(&path).unwrap().ino()
        };
        let alone = r#"{"claudeAiOauth":{"accessToken":"sk-ant-oat01-x","refreshToken":"y","expiresAt":1800003600000,"scopes":["user:inference"],"subscriptionType":"max"}}"#;
        // Top-level marker after the object (the shape Claude Code's rewrite keeps).
        let ino = put(
            r#"{"claudeAiOauth":{"accessToken":"sk-ant-oat01-x","refreshToken":"y","expiresAt":1800003600000,"scopes":["user:inference"],"subscriptionType":"max"},"_nemr_placeholder":"no Claude login on this machine yet"}"#,
        );
        assert!(
            scrub_placeholder_marker(&path).unwrap(),
            "a marker beside a real token is cleared"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), alone);
        let meta = std::fs::metadata(&path).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        assert_eq!(
            meta.ino(),
            ino,
            "cleared in place: the bind must still see it"
        );
        assert!(
            !scrub_placeholder_marker(&path).unwrap(),
            "nothing left to clear"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), alone);
        // Top-level marker before the object, and a marker nested first inside it.
        put(
            r#"{"_nemr_placeholder":"x","claudeAiOauth":{"accessToken":"a","refreshToken":"b","expiresAt":1800003600000}}"#,
        );
        assert!(scrub_placeholder_marker(&path).unwrap());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            r#"{"claudeAiOauth":{"accessToken":"a","refreshToken":"b","expiresAt":1800003600000}}"#
        );
        put(
            r#"{"claudeAiOauth":{"_nemr_placeholder":"x","accessToken":"a","refreshToken":"b","expiresAt":1800003600000}}"#,
        );
        assert!(scrub_placeholder_marker(&path).unwrap());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            r#"{"claudeAiOauth":{"accessToken":"a","refreshToken":"b","expiresAt":1800003600000}}"#
        );
        // Never touched: the placeholder (no token), a blank clear, garbage.
        for text in [
            PLACEHOLDER,
            r#"{"claudeAiOauth":{"accessToken":"","refreshToken":"","expiresAt":0}}"#,
            "garbage",
        ] {
            put(text);
            assert!(
                !scrub_placeholder_marker(&path).unwrap(),
                "left alone: {text}"
            );
            assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod e21_tests {
    use super::*;

    /// The placeholder is recognised as "no login yet" — never as expired,
    /// never as blank — and a real credential is not mistaken for it.
    #[test]
    fn the_placeholder_is_no_login_yet_and_nothing_else_is() {
        assert!(is_placeholder(PLACEHOLDER));
        assert_eq!(
            credential_facts(PLACEHOLDER).verdict(1_800_000_000),
            CredentialVerdict::NoLoginYet
        );
        let real = r#"{"claudeAiOauth":{"accessToken":"x","refreshToken":"y","expiresAt":1800003600000,"refreshTokenExpiresAt":1802000000000}}"#;
        assert!(!is_placeholder(real));
        assert!(matches!(
            credential_facts(real).verdict(1_800_000_000),
            CredentialVerdict::Fresh { .. }
        ));
        let blank = r#"{"claudeAiOauth":{"accessToken":"","refreshToken":"","expiresAt":0}}"#;
        assert_eq!(
            credential_facts(blank).verdict(1_800_000_000),
            CredentialVerdict::Blank
        );
        // The placeholder is not "dead": create must not refuse it.
        let dir = std::env::temp_dir().join(format!("nemr-e21-unit-a-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".credentials.json");
        std::fs::write(&path, PLACEHOLDER).unwrap();
        refuse_dead_credential(&path, 1_800_000_000)
            .expect("a placeholder is a fresh machine, not a dead login");
    }

    /// `ensure_host_credential_file` writes the placeholder 0600 where the
    /// path names nothing, and leaves an existing file exactly alone.
    #[test]
    fn the_placeholder_is_written_once_and_never_over_a_file() {
        use std::os::unix::fs::PermissionsExt;
        let base = std::env::temp_dir().join(format!("nemr-e21-unit-b-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let path = base.join("claude").join(".credentials.json");
        // The seam is path-only: everything below happens at this path.
        std::env::set_var("NEMR_HOST_CREDENTIALS", &path);
        let got = ensure_host_credential_file().unwrap();
        assert_eq!(got, path);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), PLACEHOLDER);
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        std::fs::write(&path, "a real login").unwrap();
        ensure_host_credential_file().unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "a real login",
            "an existing file is never touched"
        );
        std::env::remove_var("NEMR_HOST_CREDENTIALS");
        // Unset, the path is the dedicated directory under HOME again (F-14).
        assert!(host_credentials_path()
            .unwrap()
            .ends_with(".local/share/nemr/host-credential/.credentials.json"));
    }
}
