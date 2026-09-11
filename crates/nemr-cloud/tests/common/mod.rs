//! Harness for the client integration tests.
//!
//! The real sync server runs in-process (against the same Postgres the
//! nemr-sync suite uses; `DATABASE_URL` required). The CLIENT runs as separate
//! processes — the actual `nemr-cloud` binary — with an isolated state
//! directory per simulated machine, so two "machines" are two environments the
//! way they would be in life. The engine is a stub script recorded into each
//! machine's directory: these tests exercise the client's own logic
//! (crypto, lease, state, subprocess orchestration), and the real-engine flow
//! is the acceptance script's job.

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use nemr_storage::local::LocalStore;
use nemr_sync::{connect_and_migrate, router, AppState, Config, DynStore, KdfCost};

/// Start the sync server on an ephemeral port; returns its base URL and the
/// bundle-store directory (so tests can inspect stored ciphertext directly).
pub fn spawn_server(lease_ttl_secs: i64) -> (String, tempfile::TempDir) {
    // The environment first, then the server's own settings file.
    let db = nemr_sync::settings::setting("DATABASE_URL")
        .unwrap_or_else(|e| panic!("{e}\n  (scripts/setup_sync_test_db.sh starts one)"));
    let store_dir = tempfile::tempdir().unwrap();
    let store_path = store_dir.path().to_path_buf();

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let pool = connect_and_migrate(&db).await.expect("connect + migrate");
            let store: Arc<dyn DynStore> = Arc::new(LocalStore::new(store_path));
            let config = Config {
                token_ttl: time::Duration::days(1),
                lease_ttl: time::Duration::seconds(lease_ttl_secs),
                server_kdf: KdfCost {
                    m_cost: 8,
                    t_cost: 1,
                    p_cost: 1,
                },
                max_login_failures: 100,
                login_window: time::Duration::minutes(15),
                bundle_prefix: "client-test-bundles".into(),
                auth_pepper: [9u8; 32],
            };
            let state = AppState {
                pool,
                store,
                config,
            };
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tx.send(addr).unwrap();
            axum::serve(listener, router(state)).await.unwrap();
        });
    });
    let addr = rx.recv().expect("server failed to start");
    (format!("http://{addr}"), store_dir)
}

/// One simulated machine: an isolated HOME/XDG_STATE_HOME, a holder identity,
/// and a stub `nemr` whose behaviour the test controls through files.
pub struct Machine {
    pub dir: tempfile::TempDir,
    pub holder: String,
    pub server: String,
    pub email: String,
    pub password: String,
}

impl Machine {
    pub fn new(holder: &str, server: &str, email: &str, password: &str) -> Machine {
        let dir = tempfile::tempdir().unwrap();
        let m = Machine {
            dir,
            holder: holder.to_string(),
            server: server.to_string(),
            email: email.to_string(),
            password: password.to_string(),
        };
        m.write_stub_nemr();
        std::fs::create_dir_all(m.stub_dir()).unwrap();
        m
    }

    pub fn state_dir(&self) -> PathBuf {
        self.dir.path().join("state")
    }
    pub fn stub_dir(&self) -> PathBuf {
        self.dir.path().join("stub")
    }
    fn stub_nemr_path(&self) -> PathBuf {
        self.dir.path().join("nemr-stub")
    }

    /// The stub engine: `export` copies a fixture out, `import` captures what
    /// it was given, `list --json` serves a canned response. Each is driven by
    /// files in `stub_dir`, so a test states exactly what the "engine" holds.
    fn write_stub_nemr(&self) {
        let stub = self.stub_dir();
        let script = format!(
            "#!/bin/sh\n\
             set -eu\n\
             STUB={stub}\n\
             case \"$1\" in\n\
               list) cat \"$STUB/list.json\" ;;\n\
               export)\n\
                 name=$2; out=$4  # export <name> -o <path>\n\
                 cp \"$STUB/bundle.plain\" \"$out\"\n\
                 stat -c %a \"$(dirname \"$out\")\" > \"$STUB/export_dir_mode\" ;;\n\
               import)\n\
                 cp \"$2\" \"$STUB/imported.plain\"\n\
                 stat -c %a \"$(dirname \"$2\")\" > \"$STUB/import_dir_mode\"\n\
                 stat -c %a \"$2\" > \"$STUB/import_file_mode\"\n\
                 echo \"imported $2 into project (stub)\" ;;\n\
               *) echo \"stub nemr: unhandled: $*\" >&2; exit 9 ;;\n\
             esac\n",
            stub = stub.display()
        );
        std::fs::write(self.stub_nemr_path(), script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            self.stub_nemr_path(),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }

    /// Record what the stub engine "has": one stopped local project and the
    /// bundle bytes its export produces.
    pub fn stub_local_project(&self, name: &str, bundle: &[u8]) {
        std::fs::write(self.stub_dir().join("bundle.plain"), bundle).unwrap();
        std::fs::write(
            self.stub_dir().join("list.json"),
            serde_json::json!({
                "projects": [{
                    "name": name,
                    "agent": "claude",
                    "quota": "2GB",
                    "running": false,
                    "volume_path": "/x",
                    "usage_known": true,
                    "used_bytes": bundle.len(),
                    "used_percent": 1.0,
                }],
                "untracked_volumes": [],
            })
            .to_string(),
        )
        .unwrap();
    }

    pub fn stub_no_local_projects(&self) {
        std::fs::write(
            self.stub_dir().join("list.json"),
            serde_json::json!({ "projects": [], "untracked_volumes": [] }).to_string(),
        )
        .unwrap();
    }

    /// Permission bits the stub engine observed on the plaintext bundle (or its
    /// directory) at the one moment they can be observed: while it existed.
    pub fn observed_mode(&self, which: &str) -> Option<u32> {
        let raw = std::fs::read_to_string(self.stub_dir().join(which)).ok()?;
        u32::from_str_radix(raw.trim(), 8).ok()
    }

    /// What the stub engine captured from `nemr import`.
    pub fn imported_bytes(&self) -> Option<Vec<u8>> {
        std::fs::read(self.stub_dir().join("imported.plain")).ok()
    }

    /// A ready-to-run client command with this machine's environment.
    pub fn client_cmd(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_nemr-cloud"));
        cmd.args(args)
            .env("HOME", self.dir.path())
            .env("XDG_STATE_HOME", self.state_dir())
            .env("NEMR_SERVER_URL", &self.server)
            .env("NEMR_CLOUD_HOLDER", &self.holder)
            .env("NEMR_CLOUD_EMAIL", &self.email)
            .env("NEMR_CLOUD_PASSWORD", &self.password)
            .env("NEMR_CLOUD_KDF_FAST", "1")
            .env("NEMR_BIN", self.stub_nemr_path());
        cmd
    }

    /// Run a client command to completion, returning (status, stdout, stderr).
    pub fn run(&self, args: &[&str]) -> (i32, String, String) {
        let out = self
            .client_cmd(args)
            .output()
            .expect("running the client binary");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    /// Register interactively: capture the recovery code from stdout and type
    /// it back — the confirm-before-usable flow, driven exactly as a person
    /// would drive it.
    pub fn register(&self) {
        let mut child = self
            .client_cmd(&["register"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawning register");
        let stdout = child.stdout.take().unwrap();
        let mut stdin = child.stdin.take().unwrap();

        let mut code = None;
        let mut seen = String::new();
        for line in BufReader::new(stdout).lines() {
            let line = line.expect("reading register output");
            seen.push_str(&line);
            seen.push('\n');
            let candidate = line.trim();
            // The code line: groups of Crockford base32 joined by hyphens.
            if candidate.len() > 10
                && candidate
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-')
                && candidate.contains('-')
            {
                code = Some(candidate.to_string());
                writeln!(stdin, "{candidate}").expect("typing the recovery code back");
                stdin.flush().unwrap();
            }
            if line.contains("logged in as") {
                break;
            }
        }
        let status = child.wait().expect("waiting for register");
        assert!(
            status.success(),
            "register failed (code seen: {code:?}); output:\n{seen}"
        );
        assert!(code.is_some(), "no recovery code appeared; output:\n{seen}");
    }

    pub fn lease_file(&self, name: &str) -> PathBuf {
        self.state_dir()
            .join(format!("nemr/cloud/leases/{name}.json"))
    }

    pub fn lease_state(&self, name: &str) -> serde_json::Value {
        let bytes = std::fs::read(self.lease_file(name)).expect("lease state file");
        serde_json::from_slice(&bytes).expect("lease state json")
    }
}

/// The single object in the server's bundle store under `prefix`, as bytes.
/// One session ⇒ one object; more than one is a test bug worth failing loudly.
pub fn stored_ciphertext(store_dir: &Path) -> Vec<u8> {
    let mut found = Vec::new();
    let mut stack = vec![store_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                found.push(path);
            }
        }
    }
    assert_eq!(
        found.len(),
        1,
        "expected exactly one stored bundle, found: {found:?}"
    );
    std::fs::read(&found[0]).unwrap()
}

/// Kill a PID and wait for it to vanish (test cleanup / crash simulation).
pub fn kill_and_wait(pid: u32) {
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }
    for _ in 0..50 {
        if !std::path::Path::new(&format!("/proc/{pid}")).exists() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

pub fn unique_email(prefix: &str) -> String {
    format!(
        "{prefix}-{}-{}@example.com",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
}
