//! `nemr server start|stop|status` — one command to run the sync server.
//!
//! WHO THIS IS FOR. Self-hosting and development. The hosted product does not
//! need it: a normal user runs `nemr login` and `nemr push` against a server
//! somebody else operates, and never starts one. It lives on the commercial
//! half for that reason — the open engine has no idea a server exists (E-11).
//!
//! WHAT IT DOES NOT DO: install or start Postgres. It checks that Postgres
//! answers and refuses, naming the connection it tried and how to start one,
//! the same way the installer refuses a missing toolchain. Starting a database
//! on somebody's machine is a bigger promise than this command makes.
//!
//! WHERE THE KNOWLEDGE LIVES. Not here. `nemr-sync --check` runs the server's
//! own preflight — settings, store, Postgres — and reports it as `key=value`
//! lines; this command reads that report and speaks it. Two consequences worth
//! keeping: the list of what a server needs cannot drift between the thing
//! that checks and the thing that runs, and this binary (which every user
//! installs) does not link the database driver or the object-store client.
//!
//! FOREGROUND, by ruling. `start` execs the server, so the process you started
//! IS the server: Ctrl-C reaches it directly, its log is on your terminal, and
//! nothing survives the window you ran it in. A background mode is a decision
//! for the Product Owner, not a default — see docs/DECISIONS.md E-24.

use std::collections::BTreeMap;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Result};

/// The report `nemr-sync --check` prints, as a map, plus whether it exited 0.
struct Preflight {
    facts: BTreeMap<String, String>,
    ok: bool,
}

impl Preflight {
    fn get(&self, key: &str) -> Option<&str> {
        self.facts.get(key).map(String::as_str)
    }
    fn state(&self, key: &str) -> &str {
        self.get(key).unwrap_or("unknown")
    }
    /// A reported value, with its newlines put back and every line indented
    /// under the refusal it belongs to. The report is one line per fact so it
    /// can be parsed; a refusal is prose so it can be read.
    fn prose(&self, key: &str, indent: &str) -> String {
        unescape(self.state(key))
            .lines()
            .map(|l| format!("{indent}{}", l.trim_end()))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Put back the line breaks the report escaped. `\\n` is a line break and
/// `\\\\` is one backslash; anything else after a backslash is itself, so a
/// message carrying a printf format string survives the round trip intact.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Where the server binary is, and why we looked there.
///
/// A sibling of this binary first: a developer running from `target/release`
/// means that tree's server, not whatever an older `install_server.sh` left on
/// PATH — the same rule the acceptances apply to the client (F-7).
fn sync_bin() -> Result<PathBuf> {
    if let Some(explicit) = std::env::var_os("NEMR_SYNC_BIN") {
        let p = PathBuf::from(explicit);
        if p.is_file() {
            return Ok(p);
        }
        bail!("NEMR_SYNC_BIN={} is not a file", p.display());
    }
    let mut looked = Vec::new();
    if let Ok(me) = std::env::current_exe() {
        if let Some(dir) = me.parent() {
            let sibling = dir.join("nemr-sync");
            if sibling.is_file() {
                return Ok(sibling);
            }
            looked.push(sibling.display().to_string());
        }
    }
    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(':').filter(|d| !d.is_empty()) {
            let candidate = Path::new(dir).join("nemr-sync");
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    looked.push("nemr-sync on PATH".into());
    bail!(
        "the server binary was not found.\n\n  looked for:  {}\n  build it:    cargo build --release -p nemr-sync\n  or set:      NEMR_SYNC_BIN=/path/to/nemr-sync",
        looked.join("\n               ")
    )
}

/// Run the server's own preflight and parse its report.
/// How long the preflight may take before it is assumed not to be a preflight
/// at all. The database connect inside it has a five-second timeout and an
/// object store on a slow link can take a few seconds more; twenty is
/// generous, and what matters is that it is FINITE.
const CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// Run the server's own preflight and parse its report.
///
/// THIS IS WHERE A BINARY THAT DOES NOT UNDERSTAND `--check` IS CAUGHT, and it
/// has to be caught by a clock rather than by asking. A `nemr-sync` older than
/// this client ignores the argument and does what it always does: it reads the
/// settings and STARTS A SERVER. Measured here — `nemr server status`, which
/// promises to change nothing, spawned yesterday's installed binary, which
/// opened the developer's real bundle store, bound a port, and ran for three
/// minutes while this command waited for output that was never coming.
///
/// So: a deadline, and a kill. A read-only command must not be able to start a
/// server, and must not be able to wait forever for one.
fn preflight() -> Result<Preflight> {
    let bin = sync_bin()?;
    let child = Command::new(&bin)
        .arg("--check")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("could not run {} --check: {e}", bin.display()))?;
    let pid = child.id() as i32;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    let out = match rx.recv_timeout(CHECK_TIMEOUT) {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => bail!("could not read {} --check: {e}", bin.display()),
        Err(_) => {
            // It is still running. Whatever it is doing, it is not reporting.
            unsafe { libc::kill(pid, libc::SIGTERM) };
            std::thread::sleep(std::time::Duration::from_millis(200));
            unsafe { libc::kill(pid, libc::SIGKILL) };
            bail!(
                "{} did not answer `--check` within {}s, so it was stopped.\n\n  \
                 A nemr-sync older than this client does not know that argument and starts a\n  \
                 SERVER instead, which is the likeliest thing to have just happened. Build or\n  \
                 install a matching one:\n\n      \
                 cargo build --release -p nemr-sync\n      \
                 ./scripts/install_server.sh\n\n  \
                 or point NEMR_SYNC_BIN at the one you mean.",
                bin.display(),
                CHECK_TIMEOUT.as_secs()
            )
        }
    };
    let mut facts = BTreeMap::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if let Some((k, v)) = line.split_once('=') {
            facts.insert(k.to_string(), v.to_string());
        }
    }
    // `settings_state` is the report's signature: every run of `--check` that
    // got as far as reading its settings prints it, and nothing else this
    // binary prints looks like it. Without it, whatever ran was not a
    // preflight — an old binary that exited, a wrapper, the wrong file.
    if !facts.contains_key("settings_state") {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!(
            "{} did not report a preflight.\n\n  It may be older than this client, which would\n  \
             mean it does not know `--check`. Build or install a matching one:\n\n      \
             cargo build --release -p nemr-sync\n\n  it said:\n{}",
            bin.display(),
            err.trim()
        );
    }
    Ok(Preflight {
        facts,
        ok: out.status.success(),
    })
}

fn pid_file() -> PathBuf {
    crate::state::server_record_path()
}

/// Field 22 of `/proc/<pid>/stat`: the moment the process started, in clock
/// ticks since boot. It survives an exec and it is what makes a pid an
/// identity — pids are recycled, and a stop that trusts the number alone
/// eventually signals a stranger.
///
/// Parsed after the LAST `)`, because field 2 is the executable name and an
/// executable name may contain spaces and brackets.
fn start_time(pid: i32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after = &stat[stat.rfind(')')? + 1..];
    after.split_whitespace().nth(19)?.parse().ok()
}

/// A live pid whose process really is the sync server, or nothing.
///
/// The check is `/proc/<pid>/comm`, not the pid alone: pids are reused, and a
/// stop that trusts a stale number sends a signal to a stranger. This project
/// has twice killed the shell doing the signalling by being loose about that.
fn running_pid() -> Option<(i32, BTreeMap<String, String>)> {
    let text = std::fs::read_to_string(pid_file()).ok()?;
    let mut fields = BTreeMap::new();
    for line in text.lines() {
        if let Some((k, v)) = line.split_once('=') {
            fields.insert(k.to_string(), v.to_string());
        }
    }
    let pid: i32 = fields.get("pid")?.parse().ok()?;
    // THREE questions, all of them cheap, because getting this wrong means
    // signalling somebody else's process: does the pid exist, is it the sync
    // server, and is it the SAME process this record was written about?
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    if comm.trim() != "nemr-sync" {
        return None;
    }
    match (
        fields.get("starttime").and_then(|s| s.parse::<u64>().ok()),
        start_time(pid),
    ) {
        (Some(recorded), Some(actual)) if recorded != actual => None,
        _ => Some((pid, fields)),
    }
}

/// Remove a pid file whose process is gone. Verified first, never guessed: a
/// file naming a LIVE server is left exactly where it is.
fn sweep_stale_pid_file() {
    let path = pid_file();
    if path.exists() && running_pid().is_none() {
        let _ = std::fs::remove_file(&path);
    }
}

fn write_pid_file(pid: i32, addr: &str, backend: &str) -> Result<()> {
    let mut body = String::new();
    body.push_str(&format!("pid={pid}\n"));
    if let Some(t) = start_time(pid) {
        body.push_str(&format!("starttime={t}\n"));
    }
    body.push_str(&format!("addr={addr}\n"));
    body.push_str(&format!("backend={backend}\n"));
    // 0600 and an atomic rename, through the same helper the client's other
    // state goes through.
    crate::state::write_private(&pid_file(), body.as_bytes())
}

/// The file the first run asks for, with every setting named and explained.
/// Printed, never written: the pepper is the one value this command must not
/// invent (E-19), and a file it wrote for you would be a file you never read.
fn print_template(path: &str, file_reading_disabled: bool) {
    if file_reading_disabled {
        eprintln!(
            "
NEMR_SYNC_ENV_FILE is empty, so no settings file is read at all and everything
must come from the environment. Unset it to use one; the settings are the same
either way, and they are listed below."
        );
    }
    eprintln!(
        "
The server reads one file — {path} — mode 0600. Create it with the settings
below, then run this again. Every line is KEY=VALUE; anything already in your
environment wins over the file, key by key.

  # ---- required ------------------------------------------------------------

  # Postgres. This command does not install or start one; see below.
  DATABASE_URL=postgres://nemr:<password>@127.0.0.1:5433/nemr

  # The auth pepper. THERE IS NO DEFAULT AND NOTHING GENERATES ONE FOR YOU:
  # an unset pepper makes /v1/auth/params an account-enumeration oracle that
  # resets on every restart (F-89), so a server without one refuses to bind.
  # Generate it once, into the file, so it never reaches your shell history:
  #
  #   umask 077; mkdir -p \"$(dirname {path})\"
  #   printf 'NEMR_AUTH_PEPPER=%s\\n' \"$(head -c 32 /dev/urandom | base64 -w0)\" >> {path}
  #
  # For a THROWAWAY server only — tests, a demo — the one escape hatch is the
  # literal value below. It uses a random pepper per process and says so loudly
  # at every start. Never for real accounts.
  #
  #   NEMR_AUTH_PEPPER=ephemeral

  # Exactly one storage backend. Both set is refused; neither is refused.
  #
  #   a directory, for tests and single-machine self-hosting (it must exist):
  NEMR_BUNDLE_DIR=/var/lib/nemr/bundles
  #
  #   or an object store, for anything shared. All five, or none:
  # NEMR_S3_PROVIDER=r2            # r2, b2 or s3
  # NEMR_S3_BUCKET=nemr-bundles
  # NEMR_S3_ENDPOINT=https://<account>.r2.cloudflarestorage.com
  # NEMR_S3_ACCESS_KEY_ID=...
  # NEMR_S3_SECRET_ACCESS_KEY=...

  # ---- optional ------------------------------------------------------------

  # Listen address. Default 127.0.0.1:8080 — loopback. Anything reachable from
  # another machine is a deployment decision, not a default.
  # NEMR_SERVER_ADDR=127.0.0.1:8080

  # Key prefix inside the store. Default: bundles
  # NEMR_BUNDLE_PREFIX=bundles

Then:

  install -m 600 /dev/null {path}   # 0600 or tighter, or the server refuses it
  $EDITOR {path}
  nemr server start
"
    );
}

/// How to get a Postgres, said once, where it is needed. On STDERR, with the
/// refusal it belongs to: half a refusal on each stream is half a refusal.
fn postgres_help() {
    eprintln!("  For a development database, this repository starts one:");
    eprintln!();
    eprintln!("      ./scripts/setup_sync_test_db.sh");
    eprintln!();
    eprintln!("  For anything else, point DATABASE_URL at a Postgres you run.");
    eprintln!("  This command does not install or start a database — that is a");
    eprintln!("  bigger promise than starting a server.");
}

fn refuse(headline: &str) {
    eprintln!();
    eprintln!("nemr server: {headline}");
}

// --- start -------------------------------------------------------------------

pub fn start() -> Result<()> {
    let report = preflight()?;

    // 1. Settings at all. A malformed or wrong-mode sync.env is the server's
    //    own refusal, quoted rather than paraphrased.
    if report.state("settings_state") == "error" {
        refuse("the settings could not be read.");
        eprintln!();
        eprintln!("{}", report.prose("error", "  "));
        eprintln!();
        std::process::exit(1);
    }

    // 2. Nothing configured anywhere: print the file it wants, and stop. Note
    //    what this does NOT do — refuse merely because the file is missing.
    //    E-19 ruled that a per-invocation environment beats a persistent file
    //    on both halves, so an environment that carries the settings is a
    //    complete configuration and this command must not argue with it.
    let unconfigured = report.state("pepper") == "missing"
        && report.state("backend_state") == "unconfigured"
        && report.state("database_state") == "unconfigured";
    if unconfigured {
        let disabled = report.state("env_file_state") == "disabled";
        // With file reading switched off there is no path to name, so the
        // template shows the default one — the settings are the same either
        // way, and a template full of "the file NEMR_SYNC_ENV_FILE names" is
        // a template nobody can paste.
        let path = if disabled {
            "~/.config/nemr/sync.env"
        } else {
            report.get("env_file").unwrap_or("~/.config/nemr/sync.env")
        };
        refuse("there is nothing to start from — no settings file, and none in the environment.");
        print_template(path, disabled);
        std::process::exit(1);
    }

    // 3. The pepper, which is never invented here (E-19).
    if report.state("pepper") == "missing" {
        refuse("the auth pepper is not set, so the server would refuse to bind.");
        eprintln!();
        eprintln!("{}", report.prose("pepper_error", "  "));
        eprintln!();
        std::process::exit(1);
    }

    // 4. The storage backend, named and probed by the server itself.
    match report.state("backend_state") {
        "ok" => {}
        "unconfigured" => {
            refuse("no storage backend is configured.");
            eprintln!();
            eprintln!("{}", report.prose("backend_error", "  "));
            eprintln!();
            std::process::exit(1);
        }
        "unwritable" => {
            refuse(&format!(
                "the storage backend cannot be written to: {}",
                report.get("backend").unwrap_or("(not named)")
            ));
            eprintln!();
            eprintln!("{}", report.prose("backend_error", "  "));
            eprintln!();
            std::process::exit(1);
        }
        _ => {
            refuse(&format!(
                "the storage backend did not answer: {}",
                report.get("backend").unwrap_or("(not named)")
            ));
            eprintln!();
            eprintln!("{}", report.prose("backend_error", "  "));
            eprintln!();
            eprintln!("  The server probes the store before it binds, so this would have");
            eprintln!("  stopped it at start rather than at a user's first push.");
            eprintln!();
            std::process::exit(1);
        }
    }

    // 5. Postgres: named, with the password redacted, and how to get one.
    if report.state("database_state") != "ok" {
        refuse("Postgres did not answer.");
        eprintln!();
        eprintln!(
            "  tried:   {}",
            report.get("database").unwrap_or("(no DATABASE_URL)")
        );
        eprintln!(
            "  said:    {}",
            unescape(report.state("database_error")).replace('\n', "\n           ")
        );
        eprintln!();
        postgres_help();
        eprintln!();
        std::process::exit(1);
    }

    if !report.ok {
        refuse("the server's own preflight failed.");
        for (k, v) in &report.facts {
            if k.ends_with("_error") {
                eprintln!("  {k}: {v}");
            }
        }
        std::process::exit(1);
    }

    // Already up? Two servers on one address is one confusing failure.
    sweep_stale_pid_file();
    if let Some((pid, fields)) = running_pid() {
        refuse(&format!(
            "a sync server is already running (pid {pid}, {}).",
            fields
                .get("addr")
                .map(String::as_str)
                .unwrap_or("address unknown")
        ));
        eprintln!();
        eprintln!("  nemr server status    what it is doing");
        eprintln!("  nemr server stop      stop it");
        eprintln!();
        std::process::exit(1);
    }

    let addr = report.state("addr").to_string();
    if std::net::TcpListener::bind(&addr).is_err() {
        refuse(&format!("something is already listening on {addr}."));
        eprintln!();
        eprintln!("  A server started now would die at bind, and every command after it");
        eprintln!("  would be talking to the OTHER server.");
        eprintln!();
        eprintln!("      nemr server status       if this command started it");
        eprintln!(
            "      ss -ltnp \"sport = :{}\"  if something else did",
            addr.rsplit(':').next().unwrap_or("8080")
        );
        eprintln!();
        std::process::exit(1);
    }
    let backend = report.get("backend").unwrap_or("unknown").to_string();
    let bin = sync_bin()?;

    println!("nemr server: starting");
    println!("  address:   {addr}");
    println!("  storage:   {backend}");
    println!(
        "  database:  {}",
        report.get("database").unwrap_or("(unset)")
    );
    if report.state("pepper") == "ephemeral" {
        println!("  pepper:    ephemeral — THROWAWAY SERVER (E-19's escape hatch)");
    }
    println!("  Ctrl-C stops it.");
    println!();

    // The pid file names THIS process, because the next call replaces this
    // process with the server: after exec the pid is the server's pid, and
    // Ctrl-C reaches it directly rather than through a parent that would have
    // to forward signals correctly to be worth having.
    write_pid_file(std::process::id() as i32, &addr, &backend)?;
    let err = Command::new(&bin).exec();
    // exec only returns on failure.
    let _ = std::fs::remove_file(pid_file());
    bail!("could not exec {}: {err}", bin.display())
}

// --- stop --------------------------------------------------------------------

pub fn stop() -> Result<()> {
    sweep_stale_pid_file();
    let Some((pid, fields)) = running_pid() else {
        println!("nemr server: nothing to stop — no server started by this command is running.");
        println!("             (`nemr server status` also looks at the address itself.)");
        return Ok(());
    };
    let addr = fields.get("addr").map(String::as_str).unwrap_or("");
    println!("nemr server: stopping pid {pid} {addr}");
    // SIGTERM, which the server now handles as its graceful shutdown — the
    // same path Ctrl-C takes. Before that handler existed every caller's
    // SIGTERM got the default disposition: dead where it stood, mid-request.
    // It is never escalated to SIGKILL here: a server that will not stop is
    // something to look at, not something to shoot.
    if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
        bail!(
            "could not signal pid {pid}: {}",
            std::io::Error::last_os_error()
        );
    }
    for _ in 0..100 {
        if running_pid().is_none() {
            let _ = std::fs::remove_file(pid_file());
            println!("nemr server: stopped");
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    bail!(
        "pid {pid} did not stop within 10 seconds. It is still running; nothing here \
         escalates to SIGKILL. Look at it, then `kill -9 {pid}` if that is what you want."
    )
}

// --- status ------------------------------------------------------------------

pub fn status() -> Result<()> {
    sweep_stale_pid_file();
    let report = preflight()?;
    let addr = report.state("addr").to_string();

    let (running, how) = match running_pid() {
        Some((pid, _)) => (true, format!("pid {pid}, started by this command")),
        None => match answers(&addr) {
            true => (
                true,
                "something is listening and answering /health, but this command did not start it"
                    .to_string(),
            ),
            false => (false, "nothing is listening".to_string()),
        },
    };

    println!("nemr server");
    println!("  running:   {}", if running { "yes" } else { "no" });
    println!("             {how}");
    println!("  address:   {addr}");
    if running {
        println!(
            "  /health:   {}",
            if answers(&addr) {
                "answers"
            } else {
                "NO ANSWER"
            }
        );
    }
    println!(
        "  storage:   {}  ({})",
        report.get("backend").unwrap_or("not configured"),
        describe_state(report.state("backend_state"), report.get("backend_error"))
    );
    println!(
        "  database:  {}  ({})",
        report.get("database").unwrap_or("not configured"),
        describe_state(report.state("database_state"), report.get("database_error"))
    );
    println!("  pepper:    {}", report.state("pepper"));
    // Only when it matters. The lease is time-based, and a database whose clock
    // has drifted makes a lease that was just taken look long expired — which
    // reads as a lease bug and is not one. Measured here at 61 seconds on a
    // development container, costing four lease tests and an hour.
    if let Some(skew) = report
        .get("database_clock_skew_ms")
        .and_then(|v| v.parse::<i64>().ok())
    {
        if skew.abs() >= 2_000 {
            println!(
                "  clock:     the database is {:.1}s {} this machine — the lease is time-based, \
                 so this will look like a lease bug. Restarting the database usually fixes it.",
                skew.abs() as f64 / 1000.0,
                if skew > 0 { "AHEAD of" } else { "BEHIND" }
            );
        }
    }
    match report.state("env_file_state") {
        "present" => println!(
            "  settings:  {}  (keys: {})",
            report.state("env_file"),
            report.state("env_file_keys")
        ),
        "absent" => println!(
            "  settings:  the environment only — {} does not exist",
            report.state("env_file")
        ),
        _ => println!("  settings:  the environment only (NEMR_SYNC_ENV_FILE is empty)"),
    }
    // The exit code is the summary: 0 means running AND everything it depends
    // on answered, so `nemr server status && curl ...` is a sentence that says
    // what it looks like it says. Anything else is 1, and the lines above say
    // which of the five it was.
    if running && report.ok {
        Ok(())
    } else {
        std::process::exit(1)
    }
}

fn describe_state(state: &str, error: Option<&str>) -> String {
    match (state, error) {
        ("ok", _) => "answers".into(),
        (s, Some(e)) => format!("{s}: {e}"),
        (s, None) => s.into(),
    }
}

/// Does something on that address answer `/health`?
fn answers(addr: &str) -> bool {
    let url = format!("http://{addr}/health");
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .ok()
        .and_then(|c| c.get(&url).send().ok())
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::unescape;

    #[test]
    fn a_report_survives_the_round_trip() {
        // The shape the pepper's refusal actually has: a printf format string
        // inside advice that is itself several lines long.
        let original = "line one\nprintf 'K=%s\\n' \"$(head -c 32 /dev/urandom)\"\nline three";
        let reported = original.replace('\\', "\\\\").replace('\n', "\\n");
        assert!(
            !reported.contains('\n'),
            "the report is one line: {reported}"
        );
        assert_eq!(unescape(&reported), original);
    }

    #[test]
    fn a_lone_backslash_is_itself() {
        assert_eq!(unescape("C:\\path"), "C:\\path");
        assert_eq!(unescape("trailing\\"), "trailing\\");
    }
}
