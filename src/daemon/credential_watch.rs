//! The credential watcher (D-02 (f), the observability condition): every
//! rewrite of the host's credential is a real event, recorded with who could
//! have made it, when, and whether what it left behind is a credential or junk.
//!
//! Why it exists. The mount can no longer distinguish a refresh from an
//! overwrite: a session can rewrite the host's login (intended, for the refresh)
//! or replace it with garbage and lock the host out of Claude Code. That cannot
//! be prevented without breaking the refresh, so it is made *observable* — the
//! same principle as the audit trail for the privileged helper: anything with
//! the power to break the host gets a record.
//!
//! What it watches, and how it attributes. inotify is inode-based, so a write
//! through a session's bind mount raises events on the host file's inode
//! regardless of namespace. Sessions write **in place** (a rename over a mount
//! point cannot succeed, so Claude Code falls back); the host's Claude Code
//! writes by **rename**. So an in-place write while sessions are running is
//! attributed to those sessions — by name, exactly, when one is running, by
//! list when several are — and a replacement in the directory is the host's.
//! That attribution is by elimination and says so; per-writer certainty would
//! need `fanotify`, which needs root.
//!
//! The second job, same events: a host-side replacement leaves every running
//! session pinned to the old inode (F-12), so each is re-bound to the current
//! file the moment the rename lands — F-12 dies while the daemon runs, and
//! `attach` covers a rename that happened while it did not.
//!
//! Where the record goes. The daemon log, as an audit line (durable), and the
//! last event on `nemr status`. Not the per-request audit stream: a session's
//! write happens outside any request, so there is no client stream to route
//! it to — that stream is correlated by request id by design.

use std::os::unix::io::RawFd;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::auth::{CredentialFacts, CredentialVerdict};

/// What happened to the credential file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteKind {
    /// Rewritten in place — the same inode. A session's write, or a tool
    /// that does not rename.
    InPlace,
    /// A new file appeared under the name — the host's Claude Code (rename),
    /// or a login.
    Replaced,
    /// The name is gone.
    Removed,
}

/// One observed rewrite, as `status` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialWrite {
    pub at_unix: i64,
    pub kind: WriteKind,
    /// Who could have done it — see the module docs for what "could" means.
    pub by: String,
    /// What the file parses as afterwards.
    pub verdict: String,
    pub valid: bool,
}

pub type LastWrite = Arc<Mutex<Option<CredentialWrite>>>;

pub fn new_last_write() -> LastWrite {
    Arc::new(Mutex::new(None))
}

/// Attribution by elimination: who could have made a write of this kind,
/// given which sessions were running when it landed.
pub fn attribute(kind: WriteKind, running: &[String]) -> String {
    match kind {
        WriteKind::Replaced => {
            "the host (a new file under the name — a login or a host-side refresh)".to_string()
        }
        WriteKind::Removed => "the host (the file was removed)".to_string(),
        WriteKind::InPlace => match running {
            [] => "the host, in place (no session was running)".to_string(),
            [one] => format!("session {one} (the only session running)"),
            many => format!("one of the running sessions {}", many.join(", ")),
        },
    }
}

/// What the file parses as after the write, for the record.
pub fn describe(facts: CredentialFacts, now_unix: i64) -> (String, bool) {
    match facts.verdict(now_unix) {
        CredentialVerdict::Fresh { access_left } => (
            format!(
                "a valid credential (access token good for {}s)",
                access_left
            ),
            true,
        ),
        CredentialVerdict::Refreshable { .. } => (
            "a credential with a spent access token and a live refresh token".to_string(),
            true,
        ),
        CredentialVerdict::RefreshExpired { .. } => (
            "a credential whose refresh token is spent".to_string(),
            false,
        ),
        CredentialVerdict::Blank => (
            "a BLANKED credential (Claude Code's dead-token clear)".to_string(),
            false,
        ),
        CredentialVerdict::NotOauth => (
            "NOT a credential — it does not parse as one".to_string(),
            false,
        ),
        CredentialVerdict::NoLoginYet => (
            "the engine's placeholder — no login on this machine yet (E-21)".to_string(),
            false,
        ),
    }
}

/// A raw event from the inotify thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawEvent {
    pub kind: WriteKind,
}

/// Watch `file` (and its directory, for replacements) and send one
/// [`RawEvent`] per rewrite until `stop` is set. Blocking; run on a thread.
///
/// Pure plumbing, testable without a daemon: the caller decides what an event
/// means. The file watch is re-added after every replacement, because the
/// new inode is a new watch target.
pub fn watch_loop(
    file: &Path,
    stop: Arc<std::sync::atomic::AtomicBool>,
    sink: impl Fn(RawEvent),
) -> std::io::Result<()> {
    use std::sync::atomic::Ordering;
    let dir = file
        .parent()
        .ok_or_else(|| std::io::Error::other("credential path has no parent"))?;
    let name = file
        .file_name()
        .ok_or_else(|| std::io::Error::other("credential path has no file name"))?
        .to_os_string();

    let fd: RawFd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let add = |path: &Path, mask: u32| -> Option<i32> {
        let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).ok()?;
        let wd = unsafe { libc::inotify_add_watch(fd, c.as_ptr(), mask) };
        (wd >= 0).then_some(wd)
    };
    let dir_wd = add(dir, libc::IN_MOVED_TO | libc::IN_CREATE | libc::IN_DELETE)
        .ok_or_else(std::io::Error::last_os_error)?;
    let mut file_wd = add(file, libc::IN_CLOSE_WRITE);

    let mut buf = vec![0u8; 16 * 1024];
    while !stop.load(Ordering::Relaxed) {
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut pfd, 1, 200) };
        if ready <= 0 {
            continue;
        }
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n <= 0 {
            continue;
        }
        let mut off = 0usize;
        while off + std::mem::size_of::<libc::inotify_event>() <= n as usize {
            let ev: libc::inotify_event =
                unsafe { std::ptr::read_unaligned(buf.as_ptr().add(off) as *const _) };
            let name_start = off + std::mem::size_of::<libc::inotify_event>();
            let name_bytes = &buf[name_start..name_start + ev.len as usize];
            let ev_name: &std::ffi::OsStr = {
                use std::os::unix::ffi::OsStrExt;
                let end = name_bytes
                    .iter()
                    .position(|b| *b == 0)
                    .unwrap_or(name_bytes.len());
                std::ffi::OsStr::from_bytes(&name_bytes[..end])
            };
            off = name_start + ev.len as usize;

            if ev.wd == dir_wd && ev_name == name.as_os_str() {
                if ev.mask & (libc::IN_MOVED_TO | libc::IN_CREATE) != 0 {
                    // A new inode under the name: re-arm the file watch on it.
                    if let Some(old) = file_wd.take() {
                        unsafe { libc::inotify_rm_watch(fd, old) };
                    }
                    file_wd = add(file, libc::IN_CLOSE_WRITE);
                    sink(RawEvent {
                        kind: WriteKind::Replaced,
                    });
                } else if ev.mask & libc::IN_DELETE != 0 {
                    sink(RawEvent {
                        kind: WriteKind::Removed,
                    });
                }
            } else if Some(ev.wd) == file_wd && ev.mask & libc::IN_CLOSE_WRITE != 0 {
                sink(RawEvent {
                    kind: WriteKind::InPlace,
                });
            }
        }
    }
    unsafe { libc::close(fd) };
    Ok(())
}

/// A running session: its project name and its task's host pid.
pub struct RunningSession {
    pub name: String,
    pub pid: u32,
}

/// Start the watcher in the daemon: an inotify thread feeding an async task
/// that attributes, records, logs and — on a host-side replacement —
/// re-binds every running session.
pub fn spawn(
    client: Arc<crate::containerd::client::ContainerdClient>,
    host: PathBuf,
    last: LastWrite,
) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<RawEvent>();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let path = host.clone();
    std::thread::Builder::new()
        .name("nemr-credential-watch".into())
        .spawn(move || {
            if let Err(e) = watch_loop(&path, stop, |ev| {
                let _ = tx.send(ev);
            }) {
                tracing::warn!(
                    nemr_audit = "warning",
                    "[nemr] credential watcher stopped: {e:#} — rewrites of {} are no longer observed",
                    path.display()
                );
            }
        })
        .expect("spawning the credential watcher thread");

    tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            let running = running_sessions(&client).await;
            let names: Vec<String> = running.iter().map(|s| s.name.clone()).collect();
            let now = unix_now();
            // F-10: the write that just landed may be a login beside the
            // leftover marker; clear it now, at the first detection.
            match crate::auth::scrub_placeholder_marker(&host) {
                Ok(true) => tracing::info!("[nemrd] cleared the engine's placeholder marker from {} — a login landed beside it (F-10)", host.display()),
                Ok(false) => {}
                Err(e) => tracing::warn!("[nemrd] could not clear the placeholder marker from {}: {e:#}", host.display()),
            }
            let facts = crate::auth::credential_facts_at(&host);
            let (verdict, valid) = describe(facts, now);
            let by = attribute(ev.kind, &names);
            let record = CredentialWrite {
                at_unix: now,
                kind: ev.kind,
                by: by.clone(),
                verdict: verdict.clone(),
                valid,
            };
            if let Ok(mut slot) = last.lock() {
                *slot = Some(record);
            }
            // The durable record: an audit line in the daemon log. `warning`
            // so it stands out; a junk write is exactly what this exists for.
            tracing::warn!(
                nemr_audit = "warning",
                "[nemr] credential {} by {by}: the file is now {verdict}",
                match ev.kind {
                    WriteKind::InPlace => "rewritten in place",
                    WriteKind::Replaced => "replaced",
                    WriteKind::Removed => "removed",
                }
            );
            // F-12: a replacement leaves every running session on the old
            // inode. Re-bind each, in a child, and say so per session.
            if ev.kind == WriteKind::Replaced {
                for s in running {
                    let host = host.clone();
                    let container = std::path::PathBuf::from(crate::config::CONTAINER_CREDENTIALS);
                    let name = s.name.clone();
                    let res = tokio::task::spawn_blocking(move || {
                        crate::engine::credential_bind::rebind_in_child(s.pid, &host, &container)
                    })
                    .await;
                    match res {
                        Ok(Ok(())) => tracing::warn!(
                            nemr_audit = "warning",
                            "[nemr] {name}: re-bound the host's current credential into the running session (F-12)"
                        ),
                        Ok(Err(e)) => tracing::warn!(
                            nemr_audit = "warning",
                            "[nemr] {name}: could NOT re-bind the current credential ({e:#}); the session holds the old file until nemr stop/start"
                        ),
                        Err(e) => tracing::warn!(nemr_audit = "warning", "[nemr] {name}: re-bind task failed: {e}"),
                    }
                }
            }
        }
    });
}

async fn running_sessions(
    client: &crate::containerd::client::ContainerdClient,
) -> Vec<RunningSession> {
    let Ok(containers) = client.list_containers().await else {
        return vec![];
    };
    let mut out = vec![];
    for c in containers {
        let Some(name) = c.labels.get(crate::engine::project::LABEL_PROJECT).cloned() else {
            continue;
        };
        let running = client
            .task_state(&c.id)
            .await
            .map(|s| s.is_running())
            .unwrap_or(false);
        if !running {
            continue;
        }
        if let Ok(Some(pid)) = client.task_pid(&c.id).await {
            out.push(RunningSession { name, pid });
        }
    }
    out
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::CredentialExpiry;

    #[test]
    fn attribution_is_by_elimination_and_says_so() {
        assert!(attribute(WriteKind::InPlace, &[]).contains("no session was running"));
        assert_eq!(
            attribute(WriteKind::InPlace, &["demo".into()]),
            "session demo (the only session running)"
        );
        let many = attribute(WriteKind::InPlace, &["a".into(), "b".into()]);
        assert!(many.contains("one of") && many.contains("a, b"), "{many}");
        assert!(attribute(WriteKind::Replaced, &["demo".into()]).contains("the host"));
    }

    /// The verdict on the result is what makes the record actionable: junk is
    /// named as junk.
    #[test]
    fn the_record_says_whether_the_result_is_a_credential() {
        let now = 1_800_000_000;
        let good = CredentialFacts {
            access: CredentialExpiry::At(now + 3600),
            refresh: CredentialExpiry::At(now + 86_400),
            blank: false,
            placeholder: false,
        };
        assert!(describe(good, now).1);
        let junk = crate::auth::credential_facts("garbage written by a session");
        let (text, valid) = describe(junk, now);
        assert!(!valid && text.contains("NOT a credential"), "{text}");
        let blank =
            crate::auth::credential_facts(r#"{"claudeAiOauth":{"accessToken":"","expiresAt":0}}"#);
        assert!(!describe(blank, now).1);
    }

    /// The inotify plumbing, against a real file: an in-place write is one
    /// InPlace event; a rename over the name is a Replaced event, and the
    /// watcher keeps working on the new inode afterwards.
    #[test]
    fn the_watcher_reports_in_place_writes_and_replacements() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let dir = std::env::temp_dir().join(format!("nemr-credwatch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(".credentials.json");
        std::fs::write(&file, "{}").unwrap();

        let stop = Arc::new(AtomicBool::new(false));
        let seen = Arc::new(Mutex::new(Vec::<WriteKind>::new()));
        let (f, s, log) = (file.clone(), stop.clone(), seen.clone());
        let handle = std::thread::spawn(move || {
            watch_loop(&f, s, |ev| log.lock().unwrap().push(ev.kind)).unwrap();
        });
        std::thread::sleep(std::time::Duration::from_millis(300));

        // In place: open, truncate, write, close — the session's pattern.
        {
            use std::io::Write;
            let mut fh = std::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&file)
                .unwrap();
            fh.write_all(b"{\"a\":1}").unwrap();
        }
        // Replace by rename — the host's pattern.
        let tmp = dir.join(".credentials.json.tmp");
        std::fs::write(&tmp, "{\"b\":2}").unwrap();
        std::fs::rename(&tmp, &file).unwrap();
        // And in place again on the NEW inode: the re-armed watch must see it.
        {
            use std::io::Write;
            let mut fh = std::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&file)
                .unwrap();
            fh.write_all(b"{\"c\":3}").unwrap();
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
        stop.store(true, Ordering::Relaxed);
        handle.join().unwrap();

        let got = seen.lock().unwrap().clone();
        assert_eq!(
            got,
            vec![WriteKind::InPlace, WriteKind::Replaced, WriteKind::InPlace],
            "events in order: {got:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
