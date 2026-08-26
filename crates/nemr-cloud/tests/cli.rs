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
    let (server, store) = spawn_server(2); // short TTL: the crash window
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
    std::thread::sleep(std::time::Duration::from_millis(2600));
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
