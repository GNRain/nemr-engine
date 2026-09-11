//! The client commands: register, login, logout, sessions, push, pull,
//! release, and the internal lease-heartbeat holder.

use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

use std::path::Path;

use anyhow::{bail, Context, Result};
use nemr_style::{bytes as human, Progress, Tone, Voice};

use crate::api::{Api, LeaseLost};
use crate::core::{self, human_bytes, EngineOps, HeldElsewhere};
use crate::engine_cli;
use crate::keys;
use crate::state::{self, LeaseState};

/// The CLI's engine: `nemr` as a subprocess, so everything the client does
/// through it a user could do by hand.
struct CliEngine;

impl EngineOps for CliEngine {
    fn list(&self) -> Result<Vec<engine_cli::LocalProject>> {
        engine_cli::list_projects()
    }
    fn export(&self, name: &str, dest: &Path) -> Result<()> {
        engine_cli::export(name, dest)
    }
    fn import(&self, bundle: &Path, name: &str) -> Result<String> {
        // The engine prints what it created; show it, as before.
        print!("{}", engine_cli::import(bundle)?);
        Ok(name.to_string())
    }
}

/// The CLI's rendering of a held lease: the core's message plus the flag.
fn with_cli_remedy(e: anyhow::Error) -> anyhow::Error {
    match e.downcast_ref::<HeldElsewhere>() {
        Some(held) => anyhow::anyhow!(
            "session {:?} is held by {} (lease expires in {}s).\n\
             Take it over with --take-over — the other machine will be locked out of writing.",
            held.session,
            held.holder,
            held.expires_in_secs
        ),
        None => e,
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn resolve_server(flag: Option<String>) -> String {
    flag.filter(|s| !s.is_empty())
        .unwrap_or_else(core::default_server)
}

fn resolve_email(flag: Option<String>) -> Result<String> {
    if let Some(e) = flag.or_else(|| std::env::var("NEMR_CLOUD_EMAIL").ok()) {
        if !e.is_empty() {
            return Ok(e);
        }
    }
    eprint!("Email: ");
    std::io::stderr().flush().ok();
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .context("reading email")?;
    let email = line.trim().to_string();
    if email.is_empty() {
        bail!("an email is required");
    }
    Ok(email)
}

// --- register / login / logout ----------------------------------------------
//
// The CLI is a thin driver of `core`: it prompts, calls, prints. The HTTP
// surface (`serve`) is another driver of the same functions, so the two can
// never disagree about what a login or a registration IS.

pub fn register(server: Option<String>, email: Option<String>) -> Result<()> {
    let server = resolve_server(server);
    let email = resolve_email(email)?;
    let password = keys::read_password(true)?;
    let pending = core::register_begin(&server, &email, &password)?;

    // Recovery is not deferrable (E-16): show the code once, and the account
    // stays unusable until the user proves they stored it by typing it back.
    let v = Voice::for_stdout();
    println!();
    println!(
        "{}",
        v.warned("Your recovery code — the ONLY way back in if you forget your password:")
    );
    println!();
    println!(
        "    {}",
        v.paint(Tone::Pending, &pending.recovery_code.display())
    );
    println!();
    println!(
        "{}",
        v.note("Store it now: a password manager, or paper. Not this machine.")
    );
    println!(
        "{}",
        v.note("A forgotten password with no recovery code is unrecoverable — the")
    );
    println!("{}", v.note("server cannot read your data either (E-16)."));
    println!();
    // Piped stdout is block-buffered: flush so a driver (or a human paging
    // output) sees the code before we block waiting for it to be typed back.
    std::io::stdout().flush().ok();

    // Retry rather than abort (F-92). The account row already exists at this
    // point, so a single-shot confirm that exits on a typo strands the user:
    // re-running `register` hits "email already registered", and the code was
    // shown once and is now gone from the screen. The code is still in memory
    // here, so ask again — and if the user gives up, say exactly how to finish.
    let mut recovered = None;
    for attempt in 1..=5 {
        eprint!("Type the recovery code to confirm you stored it: ");
        std::io::stderr().flush().ok();
        let mut typed = String::new();
        if std::io::stdin()
            .read_line(&mut typed)
            .context("reading the recovery code")?
            == 0
        {
            break; // stdin closed
        }
        match core::register_check_code(&pending, &typed) {
            Some(mk) => {
                recovered = Some(mk);
                break;
            }
            None if attempt < 5 => {
                let ev = Voice::for_stderr();
                eprintln!(
                    "{}",
                    ev.failed("That code does not open the recovery envelope.")
                );
                eprintln!(
                    "{}",
                    ev.note("It is the code printed above; hyphens and case do not matter.")
                );
            }
            None => {}
        }
    }
    let Some(recovered) = recovered else {
        bail!("{}", core::unconfirmed_message());
    };
    let account = core::register_confirm(&pending, &recovered)?;
    println!(
        "{}",
        v.done("Account created, and the recovery code confirmed.")
    );
    println!("{}", v.field("account", &account.email, Tone::Plain));
    println!("{}", v.field("server", &account.server, Tone::Plain));
    Ok(())
}

pub fn login(server: Option<String>, email: Option<String>) -> Result<()> {
    let server = resolve_server(server);
    let email = resolve_email(email)?;
    let password = keys::read_password(false)?;
    let account = core::login(&server, &email, &password)?;
    let v = Voice::for_stdout();
    println!("{}", v.done(&format!("Logged in as {}.", account.email)));
    println!("{}", v.field("server", &account.server, Tone::Plain));
    Ok(())
}

pub fn logout() -> Result<()> {
    let report = core::logout()?;
    if let Some(e) = report.revoke_failed {
        let ev = Voice::for_stderr();
        eprintln!("{}", ev.warned("Could not revoke the token on the server."));
        eprintln!("{}", ev.note(&e.to_string()));
        eprintln!(
            "{}",
            ev.note("It expires on its own; local state is cleared regardless.")
        );
    }
    if report.was_logged_in {
        println!("{}", Voice::for_stdout().done("Logged out."));
    } else {
        println!(
            "{}",
            Voice::for_stdout().done("Not logged in — nothing to do.")
        );
    }
    Ok(())
}

// --- sessions ---------------------------------------------------------------

pub fn sessions() -> Result<()> {
    // Local projects, best-effort: sync must still show the server list on a
    // machine where the engine is absent or its daemon cannot start.
    let local = match engine_cli::list_projects() {
        Ok(list) => Some(list),
        Err(e) => {
            let ev = Voice::for_stderr();
            eprintln!(
                "{}",
                ev.warned("Local projects are unavailable; showing the server's index only.")
            );
            eprintln!("{}", ev.note(&format!("{e:#}")));
            None
        }
    };
    let rows = core::sessions(local.as_deref())?;

    if rows.is_empty() {
        let v = Voice::for_stdout();
        println!("{}", v.done("No sessions, here or on the server."));
        println!(
            "{}",
            v.field("Create one", "nemr create <name> --size 2GB", Tone::Good)
        );
        return Ok(());
    }

    println!(
        "{:<18} {:<8} {:<7} {:<9} {:<12} {:<14} OPEN ON",
        "NAME", "AGENT", "WHERE", "SIZE", "UPDATED", "LAST MACHINE"
    );
    for r in &rows {
        // A session that exists remotely but not locally is a normal state —
        // that is the whole product — so WHERE says it plainly.
        let size = r.size_bytes.map(human_bytes).unwrap_or_else(|| "-".into());
        let updated = r.updated_at_unix.map(ago).unwrap_or_else(|| "-".into());
        let machine = r.last_machine.clone().unwrap_or_else(|| "-".into());
        // Held right now, per the server: the D-03 state a user must know
        // before pulling.
        let open_on = r.held_by.clone().unwrap_or_else(|| "-".into());
        println!(
            "{:<18} {:<8} {:<7} {size:<9} {updated:<12} {machine:<14} {open_on}",
            r.name,
            r.agent,
            r.location.as_str()
        );
    }
    Ok(())
}

fn ago(unix: i64) -> String {
    let d = (now_unix() - unix).max(0);
    if d < 60 {
        format!("{d}s ago")
    } else if d < 3600 {
        format!("{}m ago", d / 60)
    } else if d < 86400 {
        format!("{}h ago", d / 3600)
    } else {
        format!("{}d ago", d / 86400)
    }
}

// --- push / pull / release ---------------------------------------------------

pub fn push(name: &str, release_after: bool, take_over: bool) -> Result<()> {
    let v = Voice::for_stdout();
    state::load_account()?; // "not logged in" before the password prompt
    let password = keys::read_password(false)?;
    // The steps are shown IN PLACE while it runs and erased when it ends, the
    // way the installer does it: what a long command leaves behind should be
    // its result, not a transcript of itself.
    let mut progress = Progress::new(v);
    let mut say = |line: &str| progress.set(line);
    let report = core::push(
        name,
        &password,
        release_after,
        take_over,
        &CliEngine,
        &mut say,
    )
    .map_err(with_cli_remedy);
    progress.clear();
    let report = report?;
    println!("{}", v.done(&format!("Pushed {name}.")));
    println!(
        "{}",
        v.field(
            "uploaded",
            &format!(
                "{} encrypted, from {}",
                human(report.ciphertext_bytes.max(0) as u64),
                human(report.plaintext_bytes as u64)
            ),
            Tone::Plain
        )
    );
    println!(
        "{}",
        v.field(
            "lease",
            &if report.released {
                "released — another machine can take this session".to_string()
            } else {
                format!("held by {}", report.holder)
            },
            if report.released {
                Tone::Plain
            } else {
                Tone::Good
            }
        )
    );
    Ok(())
}

pub fn pull(name: &str, take_over: bool) -> Result<()> {
    let v = Voice::for_stdout();
    state::load_account()?;
    let password = keys::read_password(false)?;
    let mut progress = Progress::new(v);
    let mut say = |line: &str| progress.set(line);
    let report =
        core::pull(name, &password, take_over, &CliEngine, &mut say).map_err(with_cli_remedy);
    progress.clear();
    let report = report?;
    println!("{}", v.done(&format!("Pulled {name}.")));
    println!("{}", v.field("session", &report.imported, Tone::Plain));
    println!(
        "{}",
        v.field("lease", &format!("held by {}", report.holder), Tone::Good)
    );
    println!(
        "{}",
        v.field("Next", &format!("nemr start {name}"), Tone::Good)
    );
    Ok(())
}

pub fn release(name: &str) -> Result<()> {
    let v = Voice::for_stdout();
    match core::release(name)? {
        core::ReleaseOutcome::NoLeaseHere => {
            println!("{}", v.done(&format!("Nothing to release for {name}.")));
            println!("{}", v.note("This machine holds no lease on it."));
        }
        core::ReleaseOutcome::Released => {
            println!("{}", v.done(&format!("Released the lease on {name}.")));
            println!("{}", v.note("Another machine can take this session now."));
        }
        core::ReleaseOutcome::AlreadyLost => {
            println!(
                "{}",
                v.warned(&format!("The lease on {name} was already gone."))
            );
            println!("{}", v.note("Taken over by another machine, or expired."));
        }
    }
    Ok(())
}

// --- the heartbeat holder -----------------------------------------------------

/// The detached renewal loop: the client-side half of D-03's "heartbeat to
/// renew". It refuses to outlive its lease:
///
/// - a refused renewal (409) means taken over or expired → mark `lost`, exit;
/// - a TTL's worth of failed transport means we cannot KNOW we still hold it →
///   same verdict, because "no successful heartbeat within TTL" IS lease-lost
///   (the rule the restarted-daemon case is built on);
///
/// After either, writing requires a fresh acquire — and the server's fence
/// refuses the stale credentials regardless of what this process does.
pub fn hold(name: &str, holder: &str, fence: i64, interval_ms: u64) -> Result<()> {
    let account = state::load_account()?;
    let api = Api::new(&account.server, Some(account.token));
    let mut expires_at = now_unix() + (interval_ms as i64 * 3) / 1000;

    loop {
        std::thread::sleep(std::time::Duration::from_millis(interval_ms));
        match api.heartbeat(name, holder, fence) {
            Ok(resp) => {
                expires_at = resp.expires_at_unix;
                let _ = state::save_lease(
                    name,
                    &LeaseState {
                        holder: holder.to_string(),
                        fence,
                        expires_at_unix: expires_at,
                        ttl_seconds: (interval_ms as i64 * 3) / 1000,
                        status: "held".into(),
                        holder_pid: Some(std::process::id()),
                    },
                );
            }
            Err(e) => {
                let lost = e.downcast_ref::<LeaseLost>().is_some();
                let past_ttl = now_unix() > expires_at;
                if lost || past_ttl {
                    let _ = state::save_lease(
                        name,
                        &LeaseState {
                            holder: holder.to_string(),
                            fence,
                            expires_at_unix: expires_at,
                            ttl_seconds: (interval_ms as i64 * 3) / 1000,
                            status: "lost".into(),
                            holder_pid: None,
                        },
                    );
                    // Exit non-zero: this process will not renew a lease it
                    // cannot prove it holds.
                    std::process::exit(1);
                }
                // Transient transport trouble inside the TTL: keep trying.
            }
        }
    }
}
