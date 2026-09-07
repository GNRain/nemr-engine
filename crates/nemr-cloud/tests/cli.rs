//! Client integration tests: the real binary, the real server, real detached
//! holder processes — only the engine is a stub (the acceptance script covers
//! the real-engine flow).

mod common;

use common::{kill_and_wait, spawn_server, stored_ciphertext, unique_email, Machine};

const PASSWORD: &str = "correct horse battery staple";

#[test]
fn register_login_logout_round_trip() {
    let (server, _store) = spawn_server(60);
    let m = Machine::new("machine-A", &server, &unique_email("reg"), PASSWORD);
    m.stub_no_local_projects();

    m.register();

    // Logged in: sessions works (empty, but authenticated).
    let (code, out, err) = m.run(&["sessions"]);
    assert_eq!(code, 0, "sessions after register: {err}");
    assert!(
        out.contains("no sessions"),
        "expected empty list, got: {out}"
    );

    // The account state is private to the user.
    let account = m.state_dir().join("nemr/cloud/account.json");
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&account).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600, "account file must be 0600");
    // And the master key is never in it — every byte on disk is either public
    // material or sealed. The envelope needs the password to open; grep for the
    // one thing that must be absent: any field named like a raw key.
    let text = std::fs::read_to_string(&account).unwrap();
    assert!(
        !text.contains("master") && !text.contains("\"mk\""),
        "no master-key material may be stored: {text}"
    );

    // Logout revokes: the same stored token no longer authenticates.
    let (code, _, _) = m.run(&["logout"]);
    assert_eq!(code, 0);
    let (code, _, err) = m.run(&["sessions"]);
    assert_ne!(code, 0, "sessions after logout must fail");
    assert!(
        err.contains("not logged in"),
        "should say it is not logged in: {err}"
    );

    // And login gets back in — the new-machine flow with just email+password.
    let (code, _, err) = m.run(&["login"]);
    assert_eq!(code, 0, "re-login: {err}");
    let (code, _, _) = m.run(&["sessions"]);
    assert_eq!(code, 0);
}

#[test]
fn push_then_pull_on_a_second_machine_is_byte_identical_and_encrypted() {
    let (server, store) = spawn_server(60);
    let email = unique_email("roundtrip");

    // Machine A has the project; its "bundle" is 200 KiB of random bytes.
    let a = Machine::new("machine-A", &server, &email, PASSWORD);
    let bundle: Vec<u8> = (0..200 * 1024).map(|_| rand::random::<u8>()).collect();
    a.stub_local_project("proj", &bundle);
    a.register();

    let (code, out, err) = a.run(&["push", "proj", "--release"]);
    assert_eq!(code, 0, "push: {err}");
    assert!(out.contains("pushed"), "push should report: {out}");

    // What the server stores is ciphertext: not the plaintext, not containing
    // it, and larger by the AEAD envelope overhead.
    let stored = stored_ciphertext(store.path());
    assert_ne!(stored, bundle, "the server must never hold the plaintext");
    assert!(
        stored.len() > bundle.len(),
        "ciphertext carries nonce+tag overhead"
    );
    // Spot-check containment: the plaintext's first 64 bytes must not appear.
    assert!(
        !stored.windows(64).any(|w| w == &bundle[..64]),
        "plaintext leaked into the stored object"
    );

    // Machine B: fresh state, nothing but email+password. Log in, see the
    // session, pull it. The master key travels only as the sealed envelope.
    let b = Machine::new("machine-B", &server, &email, PASSWORD);
    b.stub_no_local_projects();
    let (code, _, err) = b.run(&["login"]);
    assert_eq!(code, 0, "login on B: {err}");

    let (code, out, err) = b.run(&["sessions"]);
    assert_eq!(code, 0, "sessions on B: {err}");
    assert!(
        out.contains("proj") && out.contains("remote"),
        "B sees the session as remote: {out}"
    );

    let (code, _, err) = b.run(&["pull", "proj"]);
    assert_eq!(code, 0, "pull on B: {err}");
    assert_eq!(
        b.imported_bytes().expect("import captured a bundle"),
        bundle,
        "the pulled bundle must be byte-identical to what A exported"
    );

    // Cleanup: stop B's holder.
    if let Some(pid) = b.lease_state("proj")["holder_pid"].as_u64() {
        kill_and_wait(pid as u32);
    }
}

#[test]
fn a_wrong_password_cannot_push() {
    let (server, _store) = spawn_server(60);
    let email = unique_email("wrongpw");
    let a = Machine::new("machine-A", &server, &email, PASSWORD);
    a.stub_local_project("proj", b"data");
    a.register();

    let (code, _, err) = a
        .client_cmd(&["push", "proj"])
        .env("NEMR_CLOUD_PASSWORD", "not the password")
        .output()
        .map(|o| {
            (
                o.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&o.stdout).into_owned(),
                String::from_utf8_lossy(&o.stderr).into_owned(),
            )
        })
        .unwrap();
    assert_ne!(code, 0, "a wrong password must not push");
    assert!(
        err.contains("wrong password"),
        "the error should say so plainly: {err}"
    );
}

/// THE WP-K proof: kill the heartbeat holder mid-hold (the daemon-crash
/// analogue), let the TTL lapse, take the session over from another machine,
/// then restart the stale holder and try to write from the stale machine.
/// Neither may succeed: the restarted holder must refuse to renew, and the
/// stale machine's push must be refused — with the server's fence behind both.
#[test]
fn a_stale_machine_is_locked_out_after_takeover() {
    // The TTL must be short enough that the crash window closes quickly, and
    // long enough that B's hold survives two subprocess invocations on a busy
    // machine — the holder renews at TTL/3, so 6s gives a 3x margin measured in
    // seconds rather than in hope (F-91: control the variable, do not fight it).
    let (server, store) = spawn_server(6);
    let email = unique_email("fence");

    // A pushes and HOLDS (no --release): a heartbeat holder is now live.
    let a = Machine::new("machine-A", &server, &email, PASSWORD);
    let original: Vec<u8> = (0..64 * 1024).map(|_| rand::random::<u8>()).collect();
    a.stub_local_project("proj", &original);
    a.register();
    let (code, _, err) = a.run(&["push", "proj"]);
    assert_eq!(code, 0, "initial push: {err}");
    let lease_a = a.lease_state("proj");
    assert_eq!(lease_a["status"], "held");
    let holder_pid = lease_a["holder_pid"].as_u64().expect("holder pid") as u32;
    let fence_a = lease_a["fence"].as_i64().unwrap();
    let baseline = stored_ciphertext(store.path());

    // The daemon-crash analogue: SIGKILL the holder. No goodbye heartbeat, no
    // release — exactly what a machine losing power looks like to the server.
    kill_and_wait(holder_pid);

    // TTL lapses; machine B claims the session (plain acquire — expired is
    // free, no takeover ceremony needed).
    std::thread::sleep(std::time::Duration::from_millis(6600));
    let b = Machine::new("machine-B", &server, &email, PASSWORD);
    b.stub_no_local_projects();
    let (code, _, err) = b.run(&["login"]);
    assert_eq!(code, 0, "login on B: {err}");
    let (code, _, err) = b.run(&["pull", "proj"]);
    assert_eq!(code, 0, "pull on B acquires the expired lease: {err}");
    let fence_b = b.lease_state("proj")["fence"].as_i64().unwrap();
    assert!(fence_b > fence_a, "B's acquire must advance the fence");

    // "Restart the daemon": A's holder comes back with its saved credentials.
    // Its first renewal is refused (the fence moved on) and it EXITS declaring
    // the lease lost, rather than continuing.
    let (code, _, _) = a.run(&[
        "__hold",
        "proj",
        "--holder",
        "machine-A",
        "--fence",
        &fence_a.to_string(),
        "--interval-ms",
        "200",
    ]);
    assert_eq!(
        code, 1,
        "a restarted holder with a stale fence must refuse to continue"
    );
    assert_eq!(
        a.lease_state("proj")["status"],
        "lost",
        "the holder must record that the lease is lost"
    );

    // And the stale machine cannot write: its push is refused, naming the
    // machine that holds the session now. Give A *different* bytes so a write
    // slipping through would be visible in the stored object.
    a.stub_local_project("proj", b"stale machine's divergent work");
    let (code, _, err) = a.run(&["push", "proj"]);
    assert_ne!(code, 0, "the stale machine's push must be refused");
    assert!(
        err.contains("machine-B") && err.contains("--take-over"),
        "the refusal names the holder and the explicit escape hatch: {err}"
    );

    // The stored bundle is untouched: the refused write wrote nothing.
    assert_eq!(
        stored_ciphertext(store.path()),
        baseline,
        "a refused push must not modify the stored bundle"
    );

    // Cleanup: stop B's holder.
    if let Some(pid) = b.lease_state("proj")["holder_pid"].as_u64() {
        kill_and_wait(pid as u32);
    }
}

#[test]
fn sessions_marks_local_remote_and_both() {
    let (server, _store) = spawn_server(60);
    let email = unique_email("sessions");
    let m = Machine::new("machine-A", &server, &email, PASSWORD);
    m.stub_local_project("here-and-there", b"bytes");
    m.register();

    // Push one project so it exists on both sides; a purely local one exists
    // only in the stub's list.
    let (code, _, err) = m.run(&["push", "here-and-there", "--release"]);
    assert_eq!(code, 0, "push: {err}");

    // Re-point the stub at a list with BOTH the pushed project and a local-only
    // one.
    std::fs::write(
        m.stub_dir().join("list.json"),
        serde_json::json!({
            "projects": [
                { "name": "here-and-there", "agent": "claude", "quota": "2GB",
                  "running": false, "volume_path": "/x", "usage_known": true,
                  "used_bytes": 5, "used_percent": 1.0 },
                { "name": "local-only", "agent": "codex", "quota": "2GB",
                  "running": false, "volume_path": "/y", "usage_known": true,
                  "used_bytes": 5, "used_percent": 1.0 },
            ],
            "untracked_volumes": [],
        })
        .to_string(),
    )
    .unwrap();

    let (code, out, err) = m.run(&["sessions"]);
    assert_eq!(code, 0, "sessions: {err}");
    let row = |name: &str| {
        out.lines()
            .find(|l| l.starts_with(name))
            .unwrap_or_else(|| panic!("no row for {name} in:\n{out}"))
            .to_string()
    };
    assert!(
        row("here-and-there").contains("both"),
        "pushed project is on both sides"
    );
    assert!(
        row("local-only").contains("local"),
        "unpushed project is local-only"
    );
}

/// F-92: a push followed by a pull must not leave TWO heartbeat holders. The
/// server matches heartbeats on (holder, fence) with no notion of process, so a
/// duplicate holder renews the same lease forever while nothing references its
/// PID — and a later release kills only the one the state file names, leaving
/// the orphan to keep the lease alive and to re-create the state file it just
/// cleared.
///
/// pull is the leg that leaked: it spawned unconditionally where push guarded.
#[test]
fn a_push_then_pull_leaves_exactly_one_holder() {
    let (server, _store) = spawn_server(60);
    let email = unique_email("oneholder");
    let a = Machine::new("machine-A", &server, &email, PASSWORD);
    a.stub_local_project("proj", b"payload");
    a.register();

    let (code, _, err) = a.run(&["push", "proj"]);
    assert_eq!(code, 0, "push: {err}");
    let after_push = a.lease_state("proj")["holder_pid"].as_u64().unwrap() as u32;

    let (code, _, err) = a.run(&["pull", "proj"]);
    assert_eq!(code, 0, "pull: {err}");
    let after_pull = a.lease_state("proj")["holder_pid"].as_u64().unwrap() as u32;

    let alive = |pid: u32| std::path::Path::new(&format!("/proc/{pid}")).exists();
    let survivors: Vec<u32> = [after_push, after_pull]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .filter(|p| alive(*p))
        .collect();
    assert_eq!(
        survivors.len(),
        1,
        "push-then-pull must leave one holder, not {survivors:?} \
         (push spawned {after_push}, pull recorded {after_pull})"
    );
    assert_eq!(
        survivors[0], after_pull,
        "the survivor must be the holder the state file names"
    );
    kill_and_wait(survivors[0]);
}

/// F-92: holder identity is matched on exact argv elements, not a substring of
/// the joined cmdline. PIDs are reused; under substring matching any process
/// whose session argument merely CONTAINS ours passes the check, so releasing
/// `proj` would signal the holder of `proj-backup` — killing a lease the user
/// never touched.
///
/// The scenario forces exactly that collision: `proj`'s recorded PID is made to
/// point at a live process that is `proj-backup`'s holder.
#[test]
fn a_pid_that_belongs_to_another_sessions_holder_is_not_signalled() {
    let (server, _store) = spawn_server(60);
    let email = unique_email("subname");
    let a = Machine::new("machine-A", &server, &email, PASSWORD);
    a.register();

    // A real holder for `proj-backup`.
    a.stub_local_project("proj-backup", b"payload");
    let (code, _, err) = a.run(&["push", "proj-backup"]);
    assert_eq!(code, 0, "push proj-backup: {err}");
    let backup = a.lease_state("proj-backup").clone();
    let backup_pid = backup["holder_pid"].as_u64().unwrap() as u32;

    // Simulate PID reuse: `proj`'s lease records the pid that is in fact
    // proj-backup's holder. A substring check ("proj" is inside "proj-backup")
    // says "ours" and kills it; an exact-argv check says "not mine".
    let lease_dir = a.state_dir().join("nemr/cloud/leases");
    std::fs::create_dir_all(&lease_dir).unwrap();
    std::fs::write(
        lease_dir.join("proj.json"),
        serde_json::json!({
            "holder": "machine-A",
            "fence": backup["fence"],
            "expires_at_unix": backup["expires_at_unix"],
            "ttl_seconds": backup["ttl_seconds"],
            "status": "held",
            "holder_pid": backup_pid,
        })
        .to_string(),
    )
    .unwrap();

    let (_code, _out, _err) = a.run(&["release", "proj"]);

    assert!(
        std::path::Path::new(&format!("/proc/{backup_pid}")).exists(),
        "releasing `proj` must not signal `proj-backup`'s holder (pid {backup_pid})"
    );
    kill_and_wait(backup_pid);
}

/// F-92: the heartbeat interval comes from the lease's full TTL as the SERVER
/// reports it. Inferring it from `expires_at - now` collapses toward a hot loop
/// whenever the lease is partly elapsed.
#[test]
fn the_holder_paces_itself_by_the_servers_ttl() {
    let (server, _store) = spawn_server(30);
    let email = unique_email("pacing");
    let a = Machine::new("machine-A", &server, &email, PASSWORD);
    a.stub_local_project("proj", b"payload");
    a.register();

    let (code, _, err) = a.run(&["push", "proj"]);
    assert_eq!(code, 0, "push: {err}");

    let lease = a.lease_state("proj");
    assert_eq!(
        lease["ttl_seconds"].as_i64().unwrap(),
        30,
        "the client must record the TTL the server reported, not infer one"
    );

    let pid = lease["holder_pid"].as_u64().unwrap() as u32;
    let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap();
    let argv: Vec<String> = cmdline
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect();
    let idx = argv
        .iter()
        .position(|a| a == "--interval-ms")
        .expect("holder must be spawned with an explicit interval");
    let interval: u64 = argv[idx + 1].parse().unwrap();
    assert_eq!(
        interval, 10_000,
        "interval must be the server TTL/3 (30s/3), got {interval}ms — \
         a smaller value means it was inferred from the remaining slice"
    );
    kill_and_wait(pid);
}

/// F-92: the plaintext bundle must never be readable by other local users.
/// `tempfile::tempdir()` honours the umask (measured 0775 with a 0664 file on
/// the reference host), so the decrypted session — the very thing E-16 exists to
/// protect — sat world-readable in /tmp for the length of a push or pull.
///
/// The stub engine records the modes at the one moment they can be observed:
/// while the plaintext actually exists, with the engine looking at it.
#[test]
fn the_plaintext_bundle_is_never_readable_by_other_users() {
    let (server, _store) = spawn_server(60);
    let email = unique_email("perms");
    let a = Machine::new("machine-A", &server, &email, PASSWORD);
    a.stub_local_project("proj", b"the plaintext session");
    a.register();

    let (code, _, err) = a.run(&["push", "proj", "--release"]);
    assert_eq!(code, 0, "push: {err}");
    let export_dir = a
        .observed_mode("export_dir_mode")
        .expect("the stub engine must have observed the export directory");
    assert_eq!(
        export_dir & 0o077,
        0,
        "on push the plaintext bundle's directory was {export_dir:o} — \
         group/other must have no access at all"
    );

    let b = Machine::new("machine-B", &server, &email, PASSWORD);
    b.stub_no_local_projects();
    let (code, _, err) = b.run(&["login"]);
    assert_eq!(code, 0, "login on B: {err}");
    let (code, _, err) = b.run(&["pull", "proj"]);
    assert_eq!(code, 0, "pull on B: {err}");

    let import_dir = b
        .observed_mode("import_dir_mode")
        .expect("the stub engine must have observed the import directory");
    assert_eq!(
        import_dir & 0o077,
        0,
        "on pull the decrypted bundle's directory was {import_dir:o} — \
         any local user could have read the session"
    );

    if let Some(pid) = b.lease_state("proj")["holder_pid"].as_u64() {
        kill_and_wait(pid as u32);
    }
}

/// E-19: the server of the last successful login is remembered across
/// logout in a 0600 file, so the address is typed once — and the control
/// proves the file is what carried it: a machine with no file and no
/// `NEMR_SERVER_URL` cannot log in at all.
#[test]
fn the_server_is_remembered_across_logout_and_a_bare_machine_is_not() {
    use std::os::unix::fs::PermissionsExt;
    let (server, _store) = spawn_server(60);
    let m = Machine::new("machine-A", &server, &unique_email("remember"), PASSWORD);
    m.stub_no_local_projects();
    m.register();

    let remembered = m.state_dir().join("nemr/cloud/server-url");
    assert_eq!(
        std::fs::read_to_string(&remembered).unwrap().trim(),
        server,
        "the login's server is remembered"
    );
    assert_eq!(
        std::fs::metadata(&remembered).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let (code, _, _) = m.run(&["logout"]);
    assert_eq!(code, 0);
    assert!(remembered.exists(), "logout must not forget the server");
    assert!(
        !m.state_dir().join("nemr/cloud/account.json").exists(),
        "but the account is gone"
    );

    // Log in again with NEMR_SERVER_URL removed: only the remembered file
    // can name the server.
    let out = m
        .client_cmd(&["login"])
        .env_remove("NEMR_SERVER_URL")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "login from the remembered server: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The control: a second machine with no file and no environment has
    // only the built-in default, which nothing serves here.
    let bare = Machine::new("machine-B", &server, &m.email, PASSWORD);
    let out = bare
        .client_cmd(&["login"])
        .env_remove("NEMR_SERVER_URL")
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "a bare machine must not find the server without the file"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("127.0.0.1:8080") || err.contains("reaching the server"),
        "the refusal names the default it tried: {err}"
    );
}

/// E-19: the install script's name list is the binary's subcommand list,
/// so `nemr <cmd>` reaches every command the binary has — `ui` included —
/// through the open CLI's extension form. Red today for `ui`.
#[test]
fn the_install_list_names_every_subcommand() {
    let script = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../scripts/install_sync_client.sh"
    ))
    .unwrap();
    let names_line = script
        .lines()
        .find(|l| l.starts_with("NAMES=("))
        .expect("NAMES=(...) in the install script");
    let mut installed: Vec<&str> = names_line
        .trim_start_matches("NAMES=(")
        .trim_end_matches(')')
        .split_whitespace()
        .collect();
    installed.sort();

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_nemr-cloud"))
        .arg("--help")
        .output()
        .unwrap();
    let help = String::from_utf8_lossy(&out.stdout);
    let commands = help
        .split("Commands:")
        .nth(1)
        .expect("a Commands section")
        .split("Options:")
        .next()
        .unwrap();
    let mut visible: Vec<&str> = commands
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter(|w| !w.is_empty() && *w != "help")
        .collect();
    visible.sort();
    assert_eq!(
        installed, visible,
        "the install script lays names for every subcommand the binary shows"
    );

    // And the extension form works for `ui`: argv[0] `nemr-ui` is the `ui`
    // subcommand.
    let dir = tempfile::tempdir().unwrap();
    let link = dir.path().join("nemr-ui");
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_nemr-cloud"), &link).unwrap();
    let out = std::process::Command::new(&link)
        .arg("--help")
        .output()
        .unwrap();
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(
        help.contains("Port to bind") || help.to_lowercase().contains("port"),
        "nemr-ui --help is the ui subcommand's help: {help}"
    );
}
