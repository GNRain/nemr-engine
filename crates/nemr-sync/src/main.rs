//! The Nemr sync server binary.
//!
//! Configuration comes from the process environment and from one file the
//! server reads itself, `~/.config/nemr/sync.env` (see `settings`):
//! - `DATABASE_URL` — Postgres connection string (required).
//! - `NEMR_SERVER_ADDR` — listen address (default `127.0.0.1:8080`).
//! - `NEMR_AUTH_PEPPER` — required (E-19): the server refuses to bind
//!   without one; `ephemeral` is the loud escape hatch for a throwaway server.
//! - the storage backend (E-20): exactly one of `NEMR_BUNDLE_DIR` (an
//!   existing directory) and the `NEMR_S3_*` set (an object store).
//! - `NEMR_BUNDLE_PREFIX` — key prefix in the store (default `bundles`).
//!
//! Start-up order is the ruling's: settings, then the store opened and
//! probed, then the database migrated, then the port bound — a server that
//! is listening is one whose store answered.
//!
//! `nemr-sync --check` runs that same preflight and STOPS: it reads the
//! settings, opens and probes the store and connects to Postgres, prints one
//! `key=value` line per fact and exits 0 only if everything answered. It
//! migrates nothing and binds nothing, so it is safe to run against a server
//! that is already up. `nemr server start|status` (the commercial CLI) is its
//! caller: the knowledge of what a working server needs lives HERE, in the
//! binary that needs it, and the client reports what this says rather than
//! keeping a second, drifting copy of the same list.

use nemr_sync::settings::{self, Pepper, Settings};
use nemr_sync::{connect_and_migrate, router, store, AppState, Config};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // `--check`: the preflight, reported and then stopped. Handled before the
    // subscriber is installed so the output is the report and nothing else.
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--check") if args.len() == 1 => return check().await,
        Some("--help") | Some("-h") => {
            println!("{USAGE}");
            return Ok(());
        }
        Some(other) => {
            eprintln!("nemr-sync: unknown argument {other}\n\n{USAGE}");
            std::process::exit(2);
        }
        None => {}
    }
    // Colour only on a terminal: a journal or a captured log file gets plain
    // text, so an operator's grep (and the acceptance's) reads the line the
    // server wrote, not its escape codes.
    use std::io::IsTerminal as _;
    tracing_subscriber::fmt()
        .with_ansi(std::io::stdout().is_terminal())
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "nemr_sync=info".into()),
        )
        .init();

    let env = |k: &str| std::env::var(k).ok();
    let settings = Settings::load(&env)?;
    // Names only, never values: the file holds the pepper and a credential.
    match &settings.file {
        Some(path) if path.exists() => tracing::info!(
            file = %path.display(),
            keys = %settings.from_file.join(","),
            "settings: read from file (environment wins key by key)"
        ),
        Some(path) => {
            tracing::info!(file = %path.display(), "settings: no file (environment only)")
        }
        None => tracing::info!("settings: NEMR_SYNC_ENV_FILE is empty, environment only"),
    }
    let get = |k: &str| settings.get(k, &env);

    let database_url = get("DATABASE_URL").ok_or_else(|| {
        anyhow::anyhow!(
            "DATABASE_URL is required (environment or {})",
            settings.file_for_messages()
        )
    })?;
    let addr = get("NEMR_SERVER_ADDR").unwrap_or_else(|| "127.0.0.1:8080".into());

    let mut config = Config::default();
    if let Some(prefix) = get("NEMR_BUNDLE_PREFIX") {
        config.bundle_prefix = prefix;
    }
    match settings::pepper(&settings, &env)? {
        Pepper::Configured(p) => config.auth_pepper = p,
        Pepper::Ephemeral => tracing::warn!(
            "NEMR_AUTH_PEPPER=ephemeral — THROWAWAY SERVER: the pepper is random for this process, so \
             account-enumeration resistance at /v1/auth/params resets on restart (F-89). Never for real accounts."
        ),
    }

    // The store first (E-20): chosen from its own variables, opened, and
    // probed before anything else has a side effect.
    let choice = settings::select_storage(&get)?;
    let store = store::open_store(&choice, &get)?;
    tracing::info!(store = %store.describe(), "storage backend");
    store::preflight(store.as_ref(), &config.bundle_prefix).await?;
    tracing::info!(store = %store.describe(), "storage backend reachable");

    let pool = connect_and_migrate(&database_url).await?;

    let state = AppState {
        pool,
        store,
        config,
    };

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!(%addr, "nemr sync server listening");
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

/// Either signal that means "stop": Ctrl-C from a terminal, and SIGTERM from
/// anything that stops a service — `nemr server stop`, a systemd unit, a
/// container runtime. Without the SIGTERM arm every one of those got the
/// default disposition instead: the process dies where it stands, mid-request,
/// with no graceful drain, and the log's last line is nothing at all.
async fn shutdown_signal() {
    let mut term = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "no SIGTERM handler; Ctrl-C only");
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("shutting down");
            return;
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => tracing::info!("shutting down (interrupt)"),
        _ = term.recv() => tracing::info!("shutting down (terminate)"),
    }
}

const USAGE: &str = "\
nemr-sync — the Nemr sync server.

    nemr-sync           run the server (settings from the environment and sync.env)
    nemr-sync --check   check what a run would need, report it, and stop

Self-hosting and development only; the hosted product does not need this.
`nemr server start` is the one command that wraps both.";

/// THE PREFLIGHT, REPORTED. Everything a run needs, checked in the order a run
/// checks it, printed as `key=value` lines and never as prose — the caller is
/// `nemr server status`, and a machine reading this must not have to parse
/// English. Exit 0 only when every backing service answered.
///
/// It has NO side effects: the database is connected to and released without
/// migrating, the store is probed with the same egress-free list the server
/// uses before it binds, and nothing is created. Safe against a live server.
///
/// It never prints a secret: the database line carries the password redacted,
/// and the store describes itself as `provider:bucket` — never the endpoint,
/// which embeds the R2 account id (E-20).
async fn check() -> anyhow::Result<()> {
    let env = |k: &str| std::env::var(k).ok();
    let settings = match Settings::load(&env) {
        Ok(s) => s,
        Err(e) => {
            println!("settings_state=error");
            println!("error={}", one_line(&e.to_string()));
            std::process::exit(1);
        }
    };
    match &settings.file {
        Some(p) if p.exists() => {
            println!("env_file={}", p.display());
            println!("env_file_state=present");
            println!("env_file_keys={}", settings.from_file.join(","));
        }
        Some(p) => {
            println!("env_file={}", p.display());
            println!("env_file_state=absent");
        }
        None => println!("env_file_state=disabled"),
    }
    let get = |k: &str| settings.get(k, &env);
    println!("settings_state=ok");

    let addr = get("NEMR_SERVER_ADDR").unwrap_or_else(|| "127.0.0.1:8080".into());
    println!("addr={addr}");

    // A missing pepper is not a warning here. E-19 ruled that a server without
    // one refuses to bind, so a preflight that exited 0 on it would be telling
    // its caller that a server will start which cannot.
    let mut ok = true;
    match settings::pepper(&settings, &env) {
        Ok(Pepper::Configured(_)) => println!("pepper=configured"),
        Ok(Pepper::Ephemeral) => println!("pepper=ephemeral"),
        Err(e) => {
            ok = false;
            println!("pepper=missing");
            println!("pepper_error={}", one_line(&e.to_string()));
        }
    }

    // The store: chosen from its own variables, opened, and probed with the
    // list the server runs before it binds.
    let prefix = get("NEMR_BUNDLE_PREFIX").unwrap_or_else(|| Config::default().bundle_prefix);
    match settings::select_storage(&get) {
        Ok(choice) => match store::open_store(&choice, &get) {
            Ok(store) => {
                println!("backend={}", store.describe());
                match store::preflight(store.as_ref(), &prefix).await {
                    Ok(()) => {
                        // LISTABLE IS NOT WRITABLE. A read-only directory
                        // passes every gate the server has and fails at the
                        // first push, which is the worst place to find out.
                        // Local only: a write probe against an object store
                        // costs a class-A operation and leaves an object,
                        // which is why E-20 chose a list for reachability.
                        let mut state = "ok";
                        if let settings::StorageChoice::Local(dir) = &choice {
                            match store::writable(dir) {
                                Ok(()) => println!("backend_writable=yes"),
                                Err(e) => {
                                    ok = false;
                                    state = "unwritable";
                                    println!("backend_writable=no");
                                    println!("backend_error={}", one_line(&e.to_string()));
                                }
                            }
                        }
                        println!("backend_state={state}");
                    }
                    Err(e) => {
                        ok = false;
                        println!("backend_state=unreachable");
                        println!("backend_error={}", one_line(&e.to_string()));
                    }
                }
            }
            Err(e) => {
                ok = false;
                println!("backend_state=error");
                println!("backend_error={}", one_line(&e.to_string()));
            }
        },
        Err(e) => {
            ok = false;
            println!("backend_state=unconfigured");
            println!("backend_error={}", one_line(&e.to_string()));
        }
    }

    // Postgres: connected to and let go. Not migrated — a check that changes
    // the thing it checks is not a check.
    match get("DATABASE_URL") {
        None => {
            ok = false;
            println!("database_state=unconfigured");
            println!(
                "database_error=DATABASE_URL is required (environment or {})",
                settings.file_for_messages()
            );
        }
        Some(url) => {
            println!("database={}", redact_password(&url));
            // ONE connection, not a pool: a pool reports "pool timed out
            // waiting for an open connection" and swallows the cause, and
            // "connection refused" versus "password authentication failed" is
            // the whole difference between two very different next steps.
            use sqlx::Connection as _;
            let attempt = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                sqlx::PgConnection::connect(&url),
            )
            .await;
            match attempt {
                Ok(Ok(mut conn)) => {
                    println!("database_state=ok");
                    // THE DATABASE'S CLOCK, because the lease is time-based and
                    // the two clocks are not the same clock. Measured here
                    // (2026-09-11): the development Postgres container had
                    // drifted 61 seconds ahead of its host, and four of the six
                    // lease tests failed with "the current holder must be able
                    // to write" — a lease bug that was not a lease bug. A
                    // restart of the container fixed it and they passed twice.
                    // Reported, never enforced: a server whose database is a
                    // little ahead still works, and this is a number for the
                    // person reading the report to weigh.
                    let sent = std::time::SystemTime::now();
                    if let Ok(row) = sqlx::query_scalar::<_, f64>(
                        "select extract(epoch from clock_timestamp())::float8",
                    )
                    .fetch_one(&mut conn)
                    .await
                    {
                        let round_trip = sent.elapsed().unwrap_or_default();
                        let here = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs_f64();
                        // Half the round trip is the fairest correction we can
                        // make without a protocol for it.
                        let skew = row - (here - round_trip.as_secs_f64() / 2.0);
                        println!("database_clock_skew_ms={}", (skew * 1000.0).round() as i64);
                    }
                    let _ = conn.close().await;
                }
                Ok(Err(e)) => {
                    ok = false;
                    println!("database_state=unreachable");
                    println!(
                        "database_error={}",
                        one_line(&redact_password(&e.to_string()))
                    );
                }
                Err(_) => {
                    ok = false;
                    println!("database_state=unreachable");
                    println!("database_error=no answer within 5 seconds");
                }
            }
        }
    }

    if ok {
        Ok(())
    } else {
        std::process::exit(1)
    }
}

/// One line, always — but not one PARAGRAPH: a `key=value` report that a
/// multi-line error could split is a report a reader would misparse at exactly
/// the wrong moment, so a newline travels as the two characters `\n` and the
/// caller puts it back. The pepper's refusal is six lines of instructions; run
/// together they are unreadable, and truncated they are useless.
fn one_line(s: &str) -> String {
    // Backslashes first, or a message that legitimately contains "\n" — the
    // pepper's advice contains a printf format string that does — would come
    // out of the caller as a line break in the middle of a command.
    s.replace('\\', "\\\\")
        .replace('\r', "")
        .replace('\n', "\\n")
}

/// `postgres://user:secret@host:5433/db` -> `postgres://user:***@host:5433/db`.
/// The password is the one part of a connection string that must never be
/// printed, and this report exists to be pasted into a bug thread.
fn redact_password(s: &str) -> String {
    let Some(scheme_end) = s.find("://") else {
        return s.to_string();
    };
    let rest = &s[scheme_end + 3..];
    let Some(at) = rest.find('@') else {
        return s.to_string();
    };
    let userinfo = &rest[..at];
    match userinfo.find(':') {
        Some(colon) => format!(
            "{}{}:***@{}",
            &s[..scheme_end + 3],
            &userinfo[..colon],
            &rest[at + 1..]
        ),
        None => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::redact_password;

    #[test]
    fn a_password_never_survives_the_report() {
        assert_eq!(
            redact_password("postgres://nemr:hunter2@127.0.0.1:5433/nemr"),
            "postgres://nemr:***@127.0.0.1:5433/nemr"
        );
        // Nothing to redact, nothing changed.
        assert_eq!(
            redact_password("postgres://nemr@127.0.0.1:5433/nemr"),
            "postgres://nemr@127.0.0.1:5433/nemr"
        );
        assert_eq!(redact_password("not a url"), "not a url");
        // And the shape sqlx puts in its own errors.
        assert!(!redact_password("error with postgres://u:p@h/db in it").contains(":p@"));
    }
}
