//! `nemr` — the CLI, and the sole user interface for Phase 1 (Section 1.3).
//!
//! Subcommands arrive per milestone: `create` at Milestone 4, start/attach/stop
//! at Milestone 5, list/delete at Milestone 6.

use std::ffi::OsString;

use anyhow::Result;
use clap::{Parser, Subcommand};

use nemr_engine::daemon::client as daemon;
use nemr_engine::engine::agent::Agent;
use nemr_engine::engine::volume::{self, VolumeSize};
use nemr_engine::proto;

#[derive(Parser)]
#[command(
    name = "nemr",
    about = "Isolated, quota-bounded Claude Code environments",
    long_about = "Nemr engine — provisions isolated, resource-bounded Claude Code \
                  environments backed by rootless containerd. No Docker involved."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// Show the full provisioning trace: every privileged call with its
    /// arguments, every mount, every loop device.
    ///
    /// The default prints a summary and a one-line note whenever the privileged
    /// helper runs. This restores the detail — the same trail `NEMR_DEBUG` gives
    /// — for when you need to reconstruct exactly what happened.
    #[arg(long, short, global = true)]
    verbose: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Create a project: a quota-bounded volume plus a ready-to-start container.
    Create {
        /// Project name (lowercase letters, digits and '-'). Omitted, you are
        /// prompted (or a name is derived from the current directory).
        name: Option<String>,

        /// Storage quota, fixed at creation time. Omitted, defaults to 2GB
        /// (or you are prompted, in a terminal).
        #[arg(long, value_parser = parse_size)]
        size: Option<VolumeSize>,

        /// Coding agent to run. Omitted, defaults to Claude Code (or you are
        /// prompted, in a terminal).
        #[arg(long, value_parser = parse_agent)]
        agent: Option<Agent>,
    },

    /// Add an existing host directory as a session: copy its tree (respecting
    /// .gitignore, always excluding target/ and node_modules/, .git carried
    /// whole) and its Claude Code history, so it can be pushed and continued on
    /// another machine (E-23). The directory is copied, not moved.
    ///
    /// `adopt` is a hidden alias for one release (F-21).
    #[command(alias = "adopt")]
    Add {
        /// The host directory to add (its tree and Claude Code history).
        dir: String,

        /// Project name. Omitted, derived from the directory's basename.
        #[arg(long)]
        name: Option<String>,

        /// Storage quota, fixed at creation time. Omitted, defaults to 2GB.
        #[arg(long, value_parser = parse_size)]
        size: Option<VolumeSize>,

        /// Coding agent to run. Omitted, Claude Code.
        #[arg(long, value_parser = parse_agent)]
        agent: Option<Agent>,

        /// Skip the confirmation. Required when stdin or stderr is not a
        /// terminal, because the confirmation cannot be shown or answered there
        /// (F-15) — add never proceeds silently.
        #[arg(long)]
        yes: bool,
    },

    /// Start a project's container.
    Start { name: String },

    /// Stop a project's container.
    Stop { name: String },

    /// Open an interactive shell inside a running project.
    Attach { name: String },

    /// List all projects with status and storage usage.
    List {
        /// Machine-readable output: one JSON object with `projects` and
        /// `untracked_volumes`. The stable surface for tooling built on top of
        /// the CLI, so scripts need not scrape the human table.
        #[arg(long)]
        json: bool,
    },

    /// Delete a project and release all its resources.
    Delete {
        name: String,
        /// Skip the confirmation prompt. Required for non-interactive use.
        #[arg(long, short = 'y')]
        yes: bool,
    },

    /// Reclaim orphaned mounts, loop devices and snapshots left by a crash.
    Reconcile,

    /// Everything about one project: state, volume, base image, credential.
    Status {
        /// Project to describe.
        name: String,
    },

    /// Change which coding agent a project runs (stop it first).
    SwitchAgent {
        /// Project to change.
        name: String,
        /// The agent to switch to.
        #[arg(value_parser = parse_agent)]
        agent: Agent,
    },

    /// Restore a bundle, creating the project if it does not exist.
    ///
    /// Works standalone against a local file: no account, no network (E-11).
    ///
    ///   nemr import session.nemr              name and quota from the bundle
    ///   nemr import newname session.nemr      restore under a different name
    ///   nemr import session.nemr --size 2GB   override the recorded quota
    /// Install this project's declared packages (carried in its bundle).
    ///
    /// Explicit by design: import suggests it and never runs it, because import
    /// works offline and provisioning needs the network. Verifies each package
    /// is actually installed afterwards rather than trusting apt's exit status.
    Provision { name: String },

    Import {
        /// The bundle to read — or, when a second argument is given, the
        /// destination project name.
        bundle_or_name: String,
        /// The bundle to read, when a destination name was given first.
        bundle: Option<std::path::PathBuf>,
        /// Quota for a project this creates. Defaults to the bundle's own.
        #[arg(long)]
        size: Option<volume::VolumeSize>,
    },

    /// Export a stopped project to a portable bundle.
    ///
    /// Works standalone against a local file: no account, no network (E-11).
    Export {
        name: String,
        /// Bundle to write. Defaults to <name>.nemr in the current directory.
        #[arg(long, short = 'o')]
        output: Option<std::path::PathBuf>,
        /// Include build artifacts and caches that are excluded by default.
        #[arg(long)]
        include_build_artifacts: bool,
    },

    /// Forward a port from this host into a running session.
    ///
    /// A server started inside a session listens in the session's network
    /// namespace, so nothing on the host can reach it until a forward exists.
    ///
    ///   nemr port add web 8000              http://127.0.0.1:8000
    ///   nemr port add web 9000:8000         a different host port
    ///   nemr port add web 8000 --expose     reachable from the network
    ///   nemr port ls web
    ///   nemr port rm web 8000
    Port {
        #[command(subcommand)]
        action: PortAction,
    },

    /// Anything else: `nemr <cmd> …` runs `nemr-<cmd> …` from PATH.
    ///
    /// The cargo/git external-subcommand pattern. This is how optional tooling
    /// extends the CLI without the CLI knowing it exists — the engine carries no
    /// list of extension names and no dependency on any of them, which is what
    /// keeps the E-11 seam structural: `nemr export`/`import` work with no
    /// network and no account whether or not any extension is installed.
    #[command(external_subcommand)]
    External(Vec<OsString>),
}

#[derive(Subcommand)]
enum PortAction {
    /// Forward a host port into the session.
    Add {
        name: String,
        /// 8000, or 9000:8000 (host:container), or 0.0.0.0:9000:8000.
        port: String,
        /// Bind all interfaces instead of loopback, making the port reachable
        /// by anything that can reach this machine.
        #[arg(long)]
        expose: bool,
    },
    /// Stop forwarding a host port.
    Rm { name: String, host_port: u16 },
    /// Show a project's forwards and their URLs.
    Ls { name: String },
}

/// Is this bind address loopback? Mirrors the engine's rule (F-98): exposure is
/// "not loopback", never a comparison against one literal.
fn is_loopback(host_ip: &str) -> bool {
    host_ip
        .parse::<std::net::IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

/// The host port the user's argument resolves to, for matching the response
/// row. Parsing is the engine's job; this only needs the number.
fn parsed_host_port(arg: &str, expose: bool) -> u32 {
    nemr_engine::engine::ports::parse_port_arg(arg, expose)
        .map(|p| p.host_port as u32)
        .unwrap_or(0)
}

/// Parse `--size`, reusing the engine's own preset parsing so the CLI cannot
/// drift from what the engine and the privileged helper accept.
/// The human label for an agent id coming back over the wire.
fn agent_label(id: &str) -> String {
    id.parse::<Agent>()
        .map(|a| a.label().to_string())
        .unwrap_or_else(|_| id.to_string())
}

fn parse_agent(input: &str) -> Result<Agent, String> {
    input.parse::<Agent>().map_err(|e| e.to_string())
}

fn parse_size(input: &str) -> Result<VolumeSize, String> {
    input.parse::<VolumeSize>().map_err(|e| e.to_string())
}

use nemr_engine::interactive::{decide, Resolution};

/// A directory basename as a session name: lowercased, every run of anything
/// else collapsed to one `-`, and the leading and trailing `-` trimmed.
///
/// One function because there are two callers — the `create` prompt's
/// suggestion and `add`'s derived name — and they were not the same: without
/// the trim, a folder whose name starts with a dot (`.claude`, `.config`)
/// derived `-claude`, which the engine's `[a-z0-9][a-z0-9-]*` rule rejects, so
/// `nemr add .claude` failed deep in the daemon with a validation error instead
/// of defaulting to something usable. It also matches what the page's panel
/// fills in for the same folder.
fn name_from_basename(base: &str) -> String {
    let mut out = String::with_capacity(base.len());
    for c in base.to_ascii_lowercase().chars() {
        if c.is_ascii_alphanumeric() || c == '-' {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

/// A name suggested from the current directory — the common case is that the
/// project you want is named after where you are.
fn name_from_cwd() -> Option<String> {
    let raw = std::env::current_dir().ok()?;
    let base = raw.file_name()?.to_string_lossy().into_owned();
    // Only offer it if it is already a valid project name; never silently
    // mangle a directory name into something that only half resembles it.
    let cleaned = name_from_basename(&base);
    (nemr_engine::engine::volume::validate_name(&cleaned).is_ok()).then_some(cleaned)
}

fn resolve_name(provided: Option<String>, interactive: bool) -> Result<String> {
    // Name has no fixed default; a cwd-derived suggestion serves as the prompt
    // pre-fill but NOT as a non-interactive default — guessing a project name
    // for a script is worse than telling it to pass one.
    match decide(
        provided,
        interactive,
        None,
        "a project name (nemr create <name>)",
    ) {
        Resolution::Provided(v) => Ok(v),
        Resolution::UseDefault(v) => Ok(v),
        Resolution::MustFail { required_flag } => {
            anyhow::bail!(
                "{required_flag} is required when stdin is not a terminal.\n\
                 Run it in a terminal to be prompted, or pass the name directly."
            )
        }
        Resolution::Prompt { .. } => {
            let suggestion = name_from_cwd().unwrap_or_default();
            let input = dialoguer::Input::<String>::new()
                .with_prompt("Project name")
                .with_initial_text(suggestion)
                .interact_text()?;
            Ok(input)
        }
    }
}

/// Ask the daemon what a size may be here.
///
/// One round trip, one source: the helper's bounds and the filesystem's free
/// space. The CLI holds no copy of either.
async fn fetch_size_limits(session: &mut daemon::Session) -> Result<volume::SizeLimits> {
    let __req = session.req(proto::SizeLimitsRequest {});
    let r = session
        .client()
        .size_limits(__req)
        .await
        .map_err(status_err)?
        .into_inner();
    Ok(volume::SizeLimits {
        min: r.min_bytes,
        max: r.max_bytes,
        block: r.block_bytes,
        free: r.free_bytes,
    })
}

/// Resolve the volume size (SPEC 1.153).
///
/// `limits` comes from the daemon, which asks the privileged helper — the one
/// program that enforces the bounds — and `statvfs`, the same call `nemr
/// status` uses. So every figure printed here is a figure something will act
/// on, not an estimate this binary keeps.
///
/// NO DEFAULT IS PASSED TO `decide`. Without a terminal a missing `--size`
/// used to become 2GB silently; a scripted create that quietly picks a quota
/// is exactly the invisible default this project refuses elsewhere, so it is
/// now a refusal that names the flag (F-15's predicate, unchanged).
fn resolve_size(
    provided: Option<VolumeSize>,
    interactive: bool,
    limits: &volume::SizeLimits,
) -> Result<VolumeSize> {
    let default = VolumeSize::default_size();
    let chosen = match decide(provided, interactive, None, "--size") {
        Resolution::Provided(v) | Resolution::UseDefault(v) => v,
        Resolution::MustFail { required_flag } => {
            anyhow::bail!(
                "{required_flag} is required when stdin is not a terminal.\n\
                 Sizes run from {} to {} here, and {} is free.\n\
                 Try: nemr create <name> --size {}",
                volume::human_size(limits.min),
                volume::human_size(limits.max),
                volume::human_size(limits.free),
                default,
            )
        }
        Resolution::Prompt { .. } => ask_size(limits, default)?,
    };

    // CHECKED WHICHEVER WAY IT ARRIVED. The prompt checks as it asks so it can
    // ask again; a `--size` flag has nobody to ask, so it is checked here and
    // refused with the same figure.
    if let Some(why) = limits.refuse(chosen) {
        let v = nemr_style::Voice::for_stderr();
        eprint!(
            "{}",
            v.refusal(
                &format!("{chosen} is not a size this host can make."),
                &why,
                &format!(
                    "pick something between {} and {}:  nemr create <name> --size {}",
                    volume::human_size(limits.min),
                    volume::human_size(limits.max),
                    default
                ),
            )
        );
        std::process::exit(1);
    }

    // ROUNDED, AND SAID SO. The helper rounds down to a whole filesystem
    // block; a create that silently differs from what was typed is the thing
    // the round trip exists to prevent. Whole MB and GB are already block
    // multiples, so this only speaks up for a byte count.
    let rounded = limits.round(chosen);
    if rounded != chosen {
        let v = nemr_style::Voice::for_stdout();
        println!(
            "{}",
            v.note(&format!(
                "{chosen} rounds down to {rounded} — volumes are whole {}-byte blocks.",
                limits.block
            ))
        );
    }
    Ok(rounded)
}

/// The two questions, in the order the Product Owner asked for them: the unit,
/// then the amount.
///
/// Split in two rather than asking for "2GB" in one box because the unit is a
/// choice between two things and the amount is a number — a single free-text
/// field makes the user guess the spelling, and every guess is a refusal.
fn ask_size(limits: &volume::SizeLimits, default: VolumeSize) -> Result<VolumeSize> {
    let v = nemr_style::Voice::for_stdout();
    let units: [(&str, u64); 2] = [("MB", volume::MB), ("GB", volume::GB)];

    let picked = dialoguer::Select::new()
        .with_prompt("You'll choose how much storage this session gets. Pick a unit")
        .items(&units.iter().map(|(n, _)| *n).collect::<Vec<_>>())
        .default(1)
        .interact()?;
    let (unit_name, unit) = units[picked];

    // The room there is, in the unit just chosen, before the number is asked
    // for — so the answer is informed rather than corrected.
    let lo = limits.min.div_ceil(unit).max(1);
    let hi = (limits.max.min(limits.free)) / unit;
    println!(
        "{}",
        v.note(&format!(
            "{lo} to {hi} {unit_name} — {} free on this disk",
            volume::human_size(limits.free)
        ))
    );

    let start = (default.bytes() / unit).clamp(lo, hi.max(lo));
    loop {
        let typed: String = dialoguer::Input::new()
            .with_prompt("How much?")
            .default(start.to_string())
            .interact_text()?;
        let typed = typed.trim();
        // Digits only: the unit was already chosen, so "2GB" here would mean
        // 2GB of GB. Say that rather than parsing it into a surprise.
        let Ok(amount) = typed.parse::<u64>() else {
            println!(
                "{}",
                v.failed(&format!("{typed:?} is not a whole number of {unit_name}"))
            );
            continue;
        };
        let Some(size) = amount.checked_mul(unit).map(VolumeSize::from_bytes) else {
            println!(
                "{}",
                v.failed(&format!("{amount} {unit_name} is too large"))
            );
            continue;
        };
        match limits.refuse(size) {
            // NAMES THE FIGURE, not "invalid size": the number the user has to
            // stay under is the only part of this that helps.
            Some(why) => println!("{}", v.failed(&why)),
            None => return Ok(size),
        }
    }
}

fn resolve_agent(provided: Option<Agent>, interactive: bool) -> Result<Agent> {
    let default = Agent::default_agent();
    match decide(provided, interactive, Some(default), "--agent") {
        Resolution::Provided(v) | Resolution::UseDefault(v) => Ok(v),
        Resolution::MustFail { required_flag } => {
            anyhow::bail!("{required_flag} is required when stdin is not a terminal")
        }
        Resolution::Prompt { default } => {
            let options = Agent::all();
            let start = options
                .iter()
                .position(|a| Some(*a) == default)
                .unwrap_or(0);
            let labels: Vec<String> = options
                .iter()
                .map(|a| format!("{} — {}", a.label(), a.description()))
                .collect();
            let choice = dialoguer::Select::new()
                .with_prompt("Coding agent")
                .items(&labels)
                .default(start)
                .interact()?;
            Ok(options[choice])
        }
    }
}

/// Turn a gRPC status into an error that prints its message plainly.
///
/// The daemon puts the full engine error — including the D-08-standard
/// actionable ones — in the status message, so surfacing that verbatim keeps
/// the CLI's error quality identical to the pre-daemon version.
/// Now, in unix seconds; 0 if the clock is before the epoch (it is not).
fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// A duration for a human: the largest unit that keeps the number small.
/// "2 days", not "172800 seconds"; "6 hours", not "0 days".
fn human_duration(secs: i64) -> String {
    let secs = secs.max(0);
    let (n, unit) = if secs >= 86_400 {
        (secs / 86_400, "day")
    } else if secs >= 3_600 {
        (secs / 3_600, "hour")
    } else if secs >= 60 {
        (secs / 60, "minute")
    } else {
        (secs, "second")
    };
    format!("{n} {unit}{}", if n == 1 { "" } else { "s" })
}

/// The credential line `status` prints and the warning `attach` prints, from
/// the same facts — so they cannot disagree. The verdict is the engine's
/// (`auth::CredentialFacts::verdict`), reconstructed from the daemon's fields.
fn credential_report(d: &proto::StatusResponse, name: &str) -> (String, Option<String>) {
    use nemr_engine::auth::{CredentialExpiry, CredentialFacts, CredentialVerdict};

    if d.credential_path.is_empty() || !d.credential_present {
        // E-21: a machine that has never logged in. `start` writes the
        // placeholder and the session starts; the login happens inside it.
        return (
            format!(
                "  credential:   NO LOGIN YET on this machine — attach and run /login inside the session;\n\
                 \x20               the login is written to this host and stays here (D-02).\n\
                 \x20               Logging in spends the account's refresh token, so this account's\n\
                 \x20               OTHER machines are logged out by it (E-13 — upstream, unavoidable).\n\
                 \x20               path: {}",
                if d.credential_path.is_empty() { "(will be created at start)".to_string() } else { d.credential_path.clone() }
            ),
            Some(format!(
                "[nemr] no Claude login on this machine yet. Run /login inside this session; it opens a URL\n\
                 \x20      to sign in with and asks for the code. The login stays on this machine (D-02, E-21).\n\
                 \x20      Note: logging in spends the account's refresh token, so any OTHER machine using\n\
                 \x20      this account is logged out by it. That is Anthropic's OAuth model, not nemr's\n\
                 \x20      doing (E-13); one account cannot be live on two machines at once.\n\
                 \x20      (nemr status {name} shows the credential's state)"
            )),
        );
    }
    let at = |secs: i64| {
        if secs == 0 {
            CredentialExpiry::Unknown
        } else {
            CredentialExpiry::At(secs)
        }
    };
    let facts = CredentialFacts {
        access: at(d.credential_expires_at_secs),
        refresh: at(d.credential_refresh_expires_at_secs),
        blank: d.credential_blank,
        placeholder: !d.credential_present,
    };
    let path = &d.credential_path;
    // F-24: never "log in on this host". Since F-14 the credential nemr uses is
    // its own, and a host `claude` login writes `~/.claude`, which the engine
    // does not read — that advice sent the user somewhere that could not help.
    // The remedy is the proven one: the next create or start resets a login
    // that cannot authenticate to "no login yet", and `/login` inside the
    // session writes a new one onto this machine.
    let host_fix = format!(
        "run `nemr start {name}` (it resets a login that cannot authenticate to \"no login yet\"), \
         then `nemr attach {name}` and `/login` inside the session"
    );
    let (mut line, mut warning) = match facts.verdict(unix_now()) {
        CredentialVerdict::NoLoginYet => unreachable!("handled above"),
        CredentialVerdict::NotOauth => (
            format!("  credential:   present at {path} (no OAuth expiry to check — API-key auth or a placeholder)"),
            None,
        ),
        CredentialVerdict::Fresh { access_left } => (
            format!(
                "  credential:   present at {path}, valid — access token expires in {}; the session refreshes it",
                human_duration(access_left)
            ),
            None,
        ),
        CredentialVerdict::Refreshable { refresh_left } => (
            format!(
                "  credential:   present at {path} — access token spent; Claude Code refreshes it on next use{}",
                match refresh_left {
                    Some(left) => format!(" (refresh token valid for {})", human_duration(left)),
                    None => String::new(),
                }
            ),
            None,
        ),
        CredentialVerdict::RefreshExpired { since } => (
            format!(
                "  credential:   present at {path} — REFRESH TOKEN EXPIRED {} ago: nothing inside a session can recover it.\n\
                 \x20               Fix: {host_fix}",
                human_duration(since)
            ),
            Some(format!(
                "[nemr] this machine's Claude login is spent (its refresh token ran out {} ago) —\n\
                 \x20      often because the same account logged in on another machine (E-13).\n\
                 \x20      {host_fix}",
                human_duration(since)
            )),
        ),
        CredentialVerdict::Blank => (
            format!(
                "  credential:   present at {path} — BLANK: Claude Code cleared it after a dead refresh (revoked, or spent).\n\
                 \x20               Fix: {host_fix}"
            ),
            Some(format!(
                "[nemr] this machine's Claude login was cleared after a refresh was refused —\n\
                 \x20      revoked, or spent because the same account logged in elsewhere (E-13).\n\
                 \x20      {host_fix}"
            )),
        ),
    };
    // F-12 overlay: what the RUNNING session sees may not be what the host has.
    // The daemon re-binds it at the next attach (and its watcher does so the
    // moment a host-side replacement lands), so this is a fact for `status`,
    // not a warning for `attach` — the attach that follows repairs it.
    if d.credential_stale == 1 {
        line.push_str(&format!(
            "\n  credential:   STALE in the running session — the host replaced the file after this session \
             started (F-12);\n\
             \x20               re-bound automatically at the next `nemr attach {name}` (or: nemr stop {name} && nemr start {name})"
        ));
    }
    // D-02 (f) observability: the last rewrite the daemon saw, whoever made it.
    if d.credential_last_write_secs > 0 {
        line.push_str(&format!(
            "\n  last rewrite: {} ago by {} — {}{}",
            human_duration(unix_now() - d.credential_last_write_secs),
            d.credential_last_write_by,
            d.credential_last_write_verdict,
            if d.credential_last_write_valid {
                ""
            } else {
                "  ← NOT a usable credential: the next start resets it to \"no login yet\"; /login inside the session"
            }
        ));
    }
    let _ = &mut warning;
    (line, warning)
}

fn status_err(status: tonic::Status) -> anyhow::Error {
    anyhow::anyhow!("{}", status.message())
}

/// Client side of `nemr attach`: stream stdin and resizes to the daemon, render
/// stdout/stderr, and return the session's exit code. The terminal is entirely
/// this side's concern — raw mode, SIGWINCH, private-mode restoration — and the
/// daemon never sees it, only the messages this derives from it.
async fn attach_client(mut client: daemon::Client, name: &str) -> Result<i32> {
    use nemr_engine::engine::tty;
    use nemr_engine::proto::{
        attach_client, attach_server, AttachClient, AttachResize, AttachServer, AttachStart,
    };
    use std::io::Write;

    let interactive = tty::stdin_is_terminal();
    let (rows, cols) = tty::window_size().map(|(w, h)| (h, w)).unwrap_or((0, 0));

    // Outbound channel: the stdin thread and the resize handler feed it; tonic
    // reads it as the client->daemon stream.
    let (tx, rx) = tokio::sync::mpsc::channel::<AttachClient>(64);
    tx.send(AttachClient {
        msg: Some(attach_client::Msg::Start(AttachStart {
            name: name.to_string(),
            rows,
            cols,
            interactive,
        })),
    })
    .await
    .ok();

    let outbound = tokio_stream::wrappers::ReceiverStream::new(rx);
    let mut inbound = client
        .attach(outbound)
        .await
        .map_err(status_err)?
        .into_inner();

    // Raw mode only for a real terminal; the guard restores it on every path.
    let _raw = tty::RawMode::enable()?;

    // stdin -> Stdin messages, on a detached OS thread (a terminal read never
    // reaches EOF, so this cannot be a cancellable task — the same reasoning as
    // the pre-daemon attach). It signals StdinEof when local input ends (a pipe).
    let stdin_tx = tx.clone();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut stdin = std::io::stdin();
        let mut buf = [0u8; 8192];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) => {
                    let _ = stdin_tx.blocking_send(AttachClient {
                        msg: Some(attach_client::Msg::StdinEof(true)),
                    });
                    break;
                }
                Ok(n) => {
                    if stdin_tx
                        .blocking_send(AttachClient {
                            msg: Some(attach_client::Msg::Stdin(buf[..n].to_vec())),
                        })
                        .is_err()
                    {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // SIGWINCH -> Resize messages.
    let mut winch =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change()).ok();

    // Track private modes the container's programs set, so they can be undone.
    let mut modes = interactive.then(tty::ModeTracker::default);
    let mut exit_code: i32 = 0;

    loop {
        tokio::select! {
            msg = inbound.message() => {
                match msg.map_err(status_err)? {
                    Some(AttachServer { msg: Some(m) }) => match m {
                        attach_server::Msg::Started(_) => {}
                        attach_server::Msg::Stdout(bytes) => {
                            if let Some(t) = modes.as_mut() { t.observe(&bytes); }
                            let mut out = std::io::stdout();
                            out.write_all(&bytes).ok();
                            out.flush().ok();
                        }
                        attach_server::Msg::Stderr(bytes) => {
                            let mut err = std::io::stderr();
                            err.write_all(&bytes).ok();
                            err.flush().ok();
                        }
                        attach_server::Msg::ExitCode(code) => { exit_code = code; break; }
                        attach_server::Msg::Error(e) => {
                            drop(_raw);
                            anyhow::bail!("{e}");
                        }
                    },
                    Some(_) => {}
                    None => break,   // daemon closed the stream
                }
            }
            Some(_) = async { match winch.as_mut() { Some(w) => w.recv().await, None => None } } => {
                if let Some((w, h)) = tty::window_size() {
                    tx.send(AttachClient {
                        msg: Some(attach_client::Msg::Resize(AttachResize { rows: h, cols: w })),
                    }).await.ok();
                }
            }
        }
    }

    // Undo display modes the container left on, in one pass before the termios
    // guard drops.
    if let Some(t) = &modes {
        let restore = t.restore_sequence();
        if !restore.is_empty() {
            let mut out = std::io::stdout();
            out.write_all(restore.as_bytes()).ok();
            out.flush().ok();
        }
    }

    Ok(exit_code)
}

/// Behave like every other command-line tool when its reader goes away.
///
/// Rust sets `SIGPIPE` to `SIG_IGN` before `main`, so a write to a closed pipe
/// returns `EPIPE` and `println!` PANICS. `nemr list | head -1` therefore
/// printed a Rust panic and exited 101 in about one run in five — and the same
/// race turned a SUCCESSFUL `nemr list | grep -q <project>` into a failed
/// pipeline, which is how an acceptance came to report "pulled project not in
/// nemr list" about a project that was listed (F-109).
///
/// Restoring the default disposition makes the process die of SIGPIPE like
/// `cat` or `ls` do: quietly, with no panic and no stack trace. It does not on
/// its own make `cmd | grep -q` a safe way to ask a question — the pipeline
/// status is still non-zero — which is what `output_has` in
/// `scripts/lib/proc.sh` is for.
///
/// # Safety
///
/// `signal(2)` with `SIG_DFL` for `SIGPIPE`, before any thread is spawned and
/// before anything writes. This is the documented way to undo the Rust runtime's
/// choice for a program that is a filter.
fn restore_default_sigpipe() {
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            // ONE SHAPE FOR EVERY FAILURE, the installer's: what went wrong,
            // then the command that fixes it. The messages already carried
            // their remedies — this is what makes them look like remedies.
            let v = nemr_style::Voice::for_stderr();
            let causes: Vec<String> = e.chain().skip(1).map(|c| c.to_string()).collect();
            eprint!("{}", nemr_style::error_block(&v, &e.to_string(), &causes));
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<()> {
    restore_default_sigpipe();

    // Parse first: the subscriber's verbosity is now a flag, so it cannot be
    // installed before the flag is known. Nothing logs during parsing.
    let cli = Cli::parse();

    // Install the subscriber before anything else can log. --verbose and
    // NEMR_DEBUG=1 turn on the decision-point detail; NEMR_LOG takes a full
    // env-filter and overrides both.
    nemr_engine::observability::init(cli.verbose);
    daemon::set_verbose(cli.verbose);

    match cli.command {
        Command::Create { name, size, agent } => {
            // Resolve each field: a flag wins; otherwise prompt in a terminal,
            // or fall back to a default / fail fast without one. The never-hang
            // logic is in nemr_engine::interactive; this only renders prompts in
            // the branch it blesses.
            let interactive = nemr_engine::interactive::is_interactive();
            // THE DAEMON FIRST, before any question. The bounds come from the
            // privileged helper and the free space from statvfs, both behind
            // the daemon (E-09: the CLI has no path into the engine). Asking
            // "how much?" before those figures are in hand is asking a
            // question whose answer cannot be checked.
            let mut session = daemon::connect().await?;
            let limits = fetch_size_limits(&mut session).await?;
            let name = resolve_name(name, interactive)?;
            let size = resolve_size(size, interactive, &limits)?;
            let agent = resolve_agent(agent, interactive)?;

            let project = {
                let __req = session.req(proto::CreateRequest {
                    name,
                    size: size.to_string(),
                    agent: agent.id().to_string(),
                });
                session
                    .client()
                    .create(__req)
                    .await
                    .map_err(status_err)?
                    .into_inner()
            };
            let project_agent: Agent = project.agent.parse().unwrap_or(Agent::ClaudeCode);

            let v = nemr_style::Voice::for_stdout();
            use nemr_style::Tone;
            println!("{}", v.done(&format!("Created {}.", project.name)));
            println!(
                "{}",
                v.field("agent", &agent_label(&project.agent), Tone::Plain)
            );
            if !project_agent.portability_verified() {
                // At the point of selection, not only in docs someone may not
                // read: "implemented the same way" must not quietly become
                // "works" (E-15/F-84).
                let ev = nemr_style::Voice::for_stderr();
                eprintln!();
                eprintln!(
                    "{}",
                    ev.warned(&format!(
                        "{} support is implemented but UNVERIFIED.",
                        agent_label(&project.agent)
                    ))
                );
                eprintln!(
                    "{}",
                    ev.note("Where it stores its conversation has not been measured, so a")
                );
                eprintln!(
                    "{}",
                    ev.note("stop/restart or an export/import may silently lose it. See F-84.")
                );
            }
            println!("{}", v.field("container", &project.container_id, Tone::Dim));
            println!(
                "{}",
                v.field(
                    "volume",
                    &format!("{} ({})", project.volume_path, project.size),
                    Tone::Dim
                )
            );
            println!(
                "{}",
                v.field("status", "stopped (ready to start)", Tone::Plain)
            );
            println!(
                "{}",
                v.field("Next", &format!("nemr start {}", project.name), Tone::Good)
            );
        }

        Command::Add {
            dir,
            name,
            size,
            agent,
            yes,
        } => {
            let source = std::path::Path::new(&dir)
                .canonicalize()
                .map_err(|e| anyhow::anyhow!("the directory {dir:?} cannot be read: {e}"))?;
            let interactive = nemr_engine::interactive::is_interactive();
            // Derive the name from the directory basename when not given.
            let name = match name {
                Some(n) => n,
                None => {
                    let base = source
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let derived = name_from_basename(&base);
                    // Offer it only if it is actually a name the engine accepts;
                    // otherwise ask (or say --name is required), rather than
                    // sending a name that fails validation in the daemon.
                    let usable = Some(derived)
                        .filter(|s| nemr_engine::engine::volume::validate_name(s).is_ok());
                    resolve_name(usable, interactive)?
                }
            };
            let mut session = daemon::connect().await?;
            let limits = fetch_size_limits(&mut session).await?;
            let size = resolve_size(size, interactive, &limits)?;
            let agent = resolve_agent(agent, interactive)?;

            // F-15: add copies a whole tree, so it says what and how big and
            // asks — on a terminal. Without one it REFUSES and names the flag;
            // it never proceeds silently, and it never tries to prompt where a
            // prompt cannot be drawn.
            if !yes {
                // The plan comes from the daemon: the CLI has no path into the
                // engine (E-09), and the daemon is the one that would do the
                // copying.
                let plan = {
                    let __req = session.req(proto::AdoptRequest {
                        name: name.clone(),
                        size: size.to_string(),
                        agent: agent.id().to_string(),
                        source_dir: source.to_string_lossy().into_owned(),
                        plan_only: true,
                    });
                    session
                        .client()
                        .adopt(__req)
                        .await
                        .map_err(status_err)?
                        .into_inner()
                };
                if !interactive {
                    anyhow::bail!(
                        "adding {} would copy {} in {} file(s) into a new session, and asks \
                         before it does.\n\
                         stdin or stderr is not a terminal, so the confirmation cannot be shown \
                         or answered here.\n\
                         Re-run with --yes to add it without confirming.",
                        plan.source,
                        volume::human_bytes(plan.bytes_copied),
                        plan.files_copied
                    );
                }
                eprintln!("About to add {} as a new session:", plan.source);
                eprintln!("  name:      {name}");
                eprintln!("  quota:     {size}");
                eprintln!(
                    "  copies:    {} in {} file(s){}",
                    volume::human_bytes(plan.bytes_copied),
                    plan.files_copied,
                    if plan.git_bytes > 0 {
                        format!(
                            ", including {} of .git",
                            volume::human_bytes(plan.git_bytes)
                        )
                    } else {
                        String::new()
                    }
                );
                if plan.is_git_repo {
                    eprintln!(
                        "  excluded:  anything .gitignore ignores, and target/ and node_modules/"
                    );
                    if plan.git_dir_external {
                        eprintln!(
                            "  note:      this is a git worktree or submodule — its git directory \
                             lives outside"
                        );
                        eprintln!(
                            "             the folder, so only the .git marker travels and the \
                             session will not"
                        );
                        eprintln!(
                            "             be a working git repository (no history, no commits)."
                        );
                    }
                } else {
                    eprintln!("  excluded:  target/ and node_modules/ (not a git repo — nothing else is ignored)");
                }
                eprintln!(
                    "  history:   {} Claude Code session transcript(s) for this directory",
                    plan.history_sessions
                );
                eprintln!("  the host directory is COPIED, not moved — it stays as it is.");
                eprint!("Add it? [y/N]: ");
                std::io::Write::flush(&mut std::io::stderr())?;
                let mut answer = String::new();
                std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut answer)?;
                if !matches!(answer.trim(), "y" | "Y" | "yes" | "YES") {
                    eprintln!(
                        "{}",
                        nemr_style::Voice::for_stderr().done("Cancelled. Nothing was added.")
                    );
                    std::process::exit(1);
                }
            }

            let summary = {
                let __req = session.req(proto::AdoptRequest {
                    name,
                    size: size.to_string(),
                    agent: agent.id().to_string(),
                    source_dir: source.to_string_lossy().into_owned(),
                    plan_only: false,
                });
                session
                    .client()
                    .adopt(__req)
                    .await
                    .map_err(status_err)?
                    .into_inner()
            };
            let v = nemr_style::Voice::for_stdout();
            use nemr_style::Tone;
            println!(
                "{}",
                v.done(&format!("Added {} as {}.", summary.source, summary.name))
            );
            println!(
                "{}",
                v.field("agent", &agent_label(&summary.agent), Tone::Plain)
            );
            println!("{}", v.field("container", &summary.container_id, Tone::Dim));
            println!(
                "{}",
                v.field(
                    "copied",
                    &format!(
                        "{} files, {} (target/ and node_modules/ excluded; .git carried)",
                        summary.files_copied,
                        volume::human_bytes(summary.bytes_copied)
                    ),
                    Tone::Plain
                )
            );
            if summary.history_sessions > 0 {
                println!(
                    "{}",
                    v.field(
                        "history",
                        &format!(
                            "{} session transcript(s) carried over{}",
                            summary.history_sessions,
                            if summary.history_lines_dropped > 0 {
                                format!(" ({} torn line(s) dropped)", summary.history_lines_dropped)
                            } else {
                                String::new()
                            }
                        ),
                        Tone::Plain
                    )
                );
                if summary.history_lines_corrupt > 0 {
                    println!(
                        "{}",
                        v.field(
                            "note",
                            &format!(
                                "{} line(s) in those transcripts do not parse as JSON and were \
                                 copied as they are",
                                summary.history_lines_corrupt
                            ),
                            Tone::Pending
                        )
                    );
                    println!(
                        "{}",
                        v.note("They are not a torn last write, so they were not dropped — the")
                    );
                    println!("{}", v.note("host's copy has them too."));
                }
            } else {
                println!(
                    "{}",
                    v.field(
                        "history",
                        "none found for this directory (a fresh session)",
                        Tone::Plain
                    )
                );
            }
            println!("{}", v.field("quota", &summary.size, Tone::Plain));
            println!(
                "{}",
                v.note("The host directory was copied, not moved — it is untouched.")
            );
            println!(
                "{}",
                v.field("Next", &format!("nemr start {}", summary.name), Tone::Good)
            );
        }

        Command::Start { name } => {
            let mut session = daemon::connect().await?;
            let pid = {
                let __req = session.req(proto::StartRequest { name: name.clone() });
                session
                    .client()
                    .start(__req)
                    .await
                    .map_err(status_err)?
                    .into_inner()
                    .supervisor_pid
            };
            let v = nemr_style::Voice::for_stdout();
            println!("{}", v.done(&format!("Started {name}.")));
            println!(
                "{}",
                v.field("supervisor", &format!("pid {pid}"), nemr_style::Tone::Dim)
            );
            println!(
                "{}",
                v.field(
                    "Attach",
                    &format!("nemr attach {name}"),
                    nemr_style::Tone::Good
                )
            );
        }

        Command::Stop { name } => {
            let mut session = daemon::connect().await?;
            let outcome = {
                let __req = session.req(proto::StopRequest { name: name.clone() });
                session
                    .client()
                    .stop(__req)
                    .await
                    .map_err(status_err)?
                    .into_inner()
                    .outcome
            };
            match outcome.as_str() {
                "graceful" => {
                    let v = nemr_style::Voice::for_stdout();
                    println!("{}", v.done(&format!("Stopped {name}.")));
                    println!("{}", v.note("It shut down on its own when asked."));
                }
                "no_task" => {
                    let v = nemr_style::Voice::for_stdout();
                    println!("{}", v.done(&format!("{name} was not running.")));
                }
                "killed" => {
                    let v = nemr_style::Voice::for_stdout();
                    println!("{}", v.warned(&format!("Stopped {name}, by force.")));
                    println!(
                        "{}",
                        v.note("It ignored the request to shut down and was killed.")
                    );
                    eprintln!(
                        "[nemr] warning: the container ignored SIGTERM and was killed after the \n\
                         grace period. Its processes were given no opportunity to flush state."
                    );
                }
                "wedged" => {
                    // F-78: a distinct, surfaced condition — never a silent
                    // success. The task may still be running.
                    eprintln!(
                        "[nemr] project {name:?} did NOT stop: it is wedged in uninterruptible"
                    );
                    eprintln!(
                        "       sleep — SIGKILL cannot reap a task blocked in the kernel until"
                    );
                    eprintln!(
                        "       that operation returns (F-78). The task may still be running."
                    );
                    eprintln!(
                        "       Check `nemr status {name}` and retry; if it persists the host"
                    );
                    eprintln!("       may be under heavy load or have stuck I/O.");
                    std::process::exit(1);
                }
                other => println!("stopped project {name:?} ({other})"),
            }
        }

        Command::List { json } => {
            let mut session = daemon::connect().await?;
            let resp = {
                let __req = session.req(proto::ListRequest {});
                session
                    .client()
                    .list(__req)
                    .await
                    .map_err(status_err)?
                    .into_inner()
            };

            if json {
                // The machine-readable contract: field names are stable surface
                // for external tooling; extend, never rename.
                let out = serde_json::json!({
                    "projects": resp.projects.iter().map(|p| serde_json::json!({
                        "name": p.name,
                        "agent": p.agent,
                        "quota": p.quota,
                        "quota_bytes": p.quota_bytes,
                        "running": p.running,
                        "volume_path": p.volume_path,
                        "usage_known": p.usage_known,
                        "used_bytes": p.used_bytes,
                        "used_percent": p.used_percent,
                    })).collect::<Vec<_>>(),
                    "untracked_volumes": resp.untracked_volumes,
                });
                println!("{}", serde_json::to_string_pretty(&out)?);
                return Ok(());
            }

            let projects = resp.projects;

            let v = nemr_style::Voice::for_stdout();
            if projects.is_empty() && resp.untracked_volumes.is_empty() {
                println!("{}", v.done("No projects yet."));
                println!(
                    "{}",
                    v.field(
                        "Create one",
                        "nemr create <name> --size 2GB",
                        nemr_style::Tone::Good
                    )
                );
                return Ok(());
            }

            if !projects.is_empty() {
                println!(
                    "{:<18} {:<9} {:<18} {:<8} VOLUME",
                    "NAME", "STATUS", "USED", "QUOTA"
                );
                for p in &projects {
                    let status = if p.running { "running" } else { "stopped" };
                    let used = if p.usage_known {
                        format!(
                            "{} ({:.0}%)",
                            volume::human_bytes(p.used_bytes),
                            p.used_percent
                        )
                    } else {
                        "unmounted".to_string()
                    };
                    // THE WORDS AND THE COLUMNS ARE UNCHANGED — three
                    // acceptance scripts match `^<name> ` and one matches
                    // `^<name>  *running` on this output. Only the STATUS
                    // value is painted, and only on a terminal, so a pipe
                    // still sees exactly what it saw before.
                    println!(
                        "{:<18} {} {:<18} {:<8} {}",
                        p.name,
                        v.pad(
                            &v.paint(
                                if p.running {
                                    nemr_style::Tone::Good
                                } else {
                                    nemr_style::Tone::Plain
                                },
                                status
                            ),
                            status.len(),
                            9
                        ),
                        used,
                        p.quota,
                        p.volume_path
                    );
                }
            }

            // F-77: a listing that shows only container-backed projects is true
            // and misleading when orphaned images are filling the disk.
            let untracked = resp.untracked_volumes;
            if !untracked.is_empty() {
                println!();
                println!(
                    "{}",
                    v.warned(&format!(
                        "{} volume image(s) belong to no project and are using disk:",
                        untracked.len()
                    ))
                );
                for name in untracked.iter().take(10) {
                    println!("  {name}");
                }
                if untracked.len() > 10 {
                    println!("  ... and {} more", untracked.len() - 10);
                }
                println!(
                    "{}",
                    v.field("Reclaim them", "nemr reconcile", nemr_style::Tone::Good)
                );
            }
        }

        Command::Provision { name } => {
            let mut session = daemon::connect().await?;
            let __req = session.req(proto::ProvisionRequest { name: name.clone() });
            let resp = session
                .client()
                .provision(__req)
                .await
                .map_err(status_err)?
                .into_inner();

            if resp.installed.is_empty() && resp.failed.is_empty() {
                println!("nothing to provision: {name:?} declares no packages.");
                return Ok(());
            }
            if !resp.installed.is_empty() {
                // One line by default; the names are short and ARE the report
                // here — this command was asked for explicitly.
                println!(
                    "provisioned {}: {}",
                    resp.installed.len(),
                    resp.installed.join(", ")
                );
            }
            if !resp.failed.is_empty() {
                for f in &resp.failed {
                    eprintln!("FAILED {}: {}", f.package, f.reason);
                }
                eprintln!(
                    "{} of {} packages could not be installed. The session is usable; \
                     re-run after fixing the cause: nemr provision {name}",
                    resp.failed.len(),
                    resp.failed.len() + resp.installed.len(),
                );
                std::process::exit(1);
            }
        }

        Command::Delete { name, yes } => {
            let mut session = daemon::connect().await?;

            // AC-6.2: deletion is destructive and irreversible — the volume and
            // everything written to it goes. Confirm unless explicitly waived.
            if !yes {
                // F-15's audit: a confirmation that cannot be shown or answered
                // must refuse and name the flag, not read EOF and call it a
                // mismatch.
                if !nemr_engine::interactive::is_interactive() {
                    anyhow::bail!(
                        "deleting {name:?} destroys its volume and everything in it, and asks \
                         before it does.\n\
                         stdin or stderr is not a terminal, so the confirmation cannot be shown \
                         or answered here.\n\
                         Re-run with --yes to delete without confirming."
                    );
                }
                let projects = {
                    let __req = session.req(proto::ListRequest {});
                    session
                        .client()
                        .list(__req)
                        .await
                        .map_err(status_err)?
                        .into_inner()
                        .projects
                };
                let target = projects.iter().find(|p| p.name == name);
                match target {
                    Some(p) => {
                        let used = if p.usage_known {
                            volume::human_bytes(p.used_bytes)
                        } else {
                            "unknown".into()
                        };
                        let ev = nemr_style::Voice::for_stderr();
                        eprintln!("{}", ev.warned(&format!("About to delete {name}:")));
                        eprintln!(
                            "{}",
                            ev.field("container", &p.container_id, nemr_style::Tone::Dim)
                        );
                        eprintln!(
                            "  volume:    {} ({} used of {})",
                            p.volume_path, used, p.quota
                        );
                        eprintln!(
                            "  status:    {}",
                            if p.running {
                                "running (will be stopped)"
                            } else {
                                "stopped"
                            }
                        );
                        eprintln!();
                        eprintln!(
                            "{}",
                            ev.paint(
                                nemr_style::Tone::Bad,
                                "This permanently destroys the volume and everything in it."
                            )
                        );
                    }
                    None => eprintln!("About to delete project {name:?} (details unavailable)."),
                }
                eprint!("Type the project name to confirm: ");
                std::io::Write::flush(&mut std::io::stderr())?;

                let mut answer = String::new();
                std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut answer)?;
                if answer.trim() != name {
                    eprintln!(
                        "{}",
                        nemr_style::Voice::for_stderr().done(&format!(
                            "Cancelled — that did not match {name}. Nothing was deleted."
                        ))
                    );
                    std::process::exit(1);
                }
            }

            {
                let __req = session.req(proto::DeleteRequest { name: name.clone() });
                session.client().delete(__req).await.map_err(status_err)?
            };
            println!(
                "{}",
                nemr_style::Voice::for_stdout().done(&format!("Deleted {name}."))
            );
        }

        Command::Reconcile => {
            let mut session = daemon::connect().await?;
            let report = {
                let __req = session.req(proto::ReconcileRequest {});
                session
                    .client()
                    .reconcile(__req)
                    .await
                    .map_err(status_err)?
                    .into_inner()
            };
            if report.released.is_empty()
                && report.not_released.is_empty()
                && report.snapshots_removed.is_empty()
                && report.orphan_backing_files.is_empty()
                && report.stale_forwards.is_empty()
                && report.unattributable_forwards.is_empty()
            {
                let v = nemr_style::Voice::for_stdout();
                println!("{}", v.done("Nothing to reconcile."));
                println!(
                    "{}",
                    v.note("No orphaned mounts, loop devices, snapshots or port forwards.")
                );
            } else {
                let v = nemr_style::Voice::for_stdout();
                use nemr_style::Tone;
                let reclaimed = report.released.len()
                    + report.snapshots_removed.len()
                    + report.stale_forwards.len();
                let stuck = report.not_released.len()
                    + report.orphan_backing_files.len()
                    + report.unattributable_forwards.len();
                if stuck == 0 {
                    println!("{}", v.done(&format!("Reclaimed {reclaimed}.")));
                } else {
                    println!(
                        "{}",
                        v.warned(&format!("Reclaimed {reclaimed}; {stuck} need you to look."))
                    );
                }
                println!();
                for name in &report.released {
                    println!(
                        "{}",
                        v.field(
                            "released",
                            &format!("{name} (mount + loop device)"),
                            Tone::Good
                        )
                    );
                }
                for name in &report.not_released {
                    println!("{}", v.field("NOT released", name, Tone::Bad));
                    println!(
                        "{}",
                        v.note(
                            "The helper reported success but the host still shows it mounted or"
                        )
                    );
                    println!(
                        "{}",
                        v.note("loop-attached, so that disk space is NOT reclaimed.")
                    );
                    println!(
                        "{}",
                        v.field("Check", &format!("losetup -a | grep {name}"), Tone::Good)
                    );
                }
                for key in &report.snapshots_removed {
                    println!(
                        "{}",
                        v.field("removed", &format!("orphan snapshot {key}"), Tone::Good)
                    );
                }
                for name in &report.orphan_backing_files {
                    println!(
                        "{}",
                        v.field(
                            "kept",
                            &format!("the backing file for {name}"),
                            Tone::Pending
                        )
                    );
                    println!(
                        "{}",
                        v.note(
                            "Its mount and loop device are released, but the file may hold data."
                        )
                    );
                    println!("{}", v.note("Remove it deliberately if you are sure."));
                }
                for f in &report.stale_forwards {
                    println!(
                        "{}",
                        v.field("reclaimed", &format!("port forward {f}"), Tone::Good)
                    );
                    println!(
                        "{}",
                        v.note("The project is stopped, so this was nemr's own leftover.")
                    );
                }
                for f in &report.unattributable_forwards {
                    println!(
                        "LEFT ALONE: port forward {f} matches no project declaration, so nemr \
                         cannot prove it created it and will not remove it. If it is yours, \
                         that is correct. If it is a nemr orphan, remove it deliberately:\n  \
                         rootlessctl --socket=$XDG_RUNTIME_DIR/containerd-rootless/api.sock \
                         remove-ports <id>"
                    );
                }
            }
        }

        Command::Status { name } => {
            let mut session = daemon::connect().await?;
            let d = {
                let __req = session.req(proto::StatusRequest { name: name.clone() });
                session
                    .client()
                    .status(__req)
                    .await
                    .map_err(status_err)?
                    .into_inner()
            };

            let v = nemr_style::Voice::for_stdout();
            use nemr_style::Tone;
            // The result first: the state of the thing, before the detail.
            if d.running {
                println!("{}", v.done(&format!("{} is running.", d.name)));
            } else {
                println!("{}", v.done(&format!("{} is stopped.", d.name)));
            }
            println!("  agent:        {}", agent_label(&d.agent));
            println!(
                "  state:        {}",
                v.paint(
                    if d.running { Tone::Good } else { Tone::Plain },
                    if d.running { "running" } else { "stopped" }
                )
            );
            println!("  container:    {}", d.container_id);

            let usage = if d.usage_known {
                format!(
                    "{} of {} ({:.0}%)",
                    volume::human_bytes(d.used_bytes),
                    d.quota,
                    d.used_percent
                )
            } else {
                format!("unmounted (quota {})", d.quota)
            };
            println!("  volume:       {}", d.mount_point);
            println!("  usage:        {usage}");
            println!(
                "  image file:   {} ({})",
                d.image_file,
                if d.image_present {
                    v.paint(Tone::Plain, "present")
                } else {
                    v.paint(Tone::Bad, "MISSING")
                }
            );
            println!(
                "  loop device:  {}",
                if d.loop_device.is_empty() {
                    "none"
                } else {
                    &d.loop_device
                }
            );

            // F-28: the question that cost the most time to answer by hand.
            match d.mount_check {
                -1 => println!("  mount check:  n/a (not mounted)"),
                1 => println!(
                    "  mount check:  {} (backed by this project's image)",
                    v.paint(Tone::Good, "ok")
                ),
                _ => println!(
                    "  mount check:  {} — mounted filesystem is backed by {}\n\
                     \x20               Run `nemr reconcile`, then start again.",
                    v.paint(Tone::Bad, "WRONG VOLUME"),
                    if d.mounted_image.is_empty() {
                        "something that is not a loop device".to_string()
                    } else {
                        d.mounted_image.clone()
                    }
                ),
            }

            // "What's my URL" is the question people actually ask when a dev
            // server is running, so status answers it rather than making them
            // reconstruct it from a port number.
            if d.ports.is_empty() {
                println!("  ports:        none forwarded (nemr port add {name} 8000)");
            } else {
                for (i, p) in d.ports.iter().enumerate() {
                    let label = if i == 0 {
                        "  ports:      "
                    } else {
                        "              "
                    };
                    let exposed = if p.host_ip == "0.0.0.0" {
                        "  (exposed to the network)"
                    } else {
                        ""
                    };
                    println!(
                        "{label}  {} -> container {}{exposed}",
                        p.url, p.container_port
                    );
                }
            }

            println!("  base image:   {}", d.base_image);
            println!(
                "  base digest:  {}",
                if d.base_image_digest.is_empty() {
                    "NOT PRESENT on this host"
                } else {
                    &d.base_image_digest
                }
            );

            // The expired-credential failure took three steps to identify; this
            // is the line that would have made it one. Then it happened again
            // with the line present, because it reported presence ("last
            // written 0 days ago" — true) instead of validity. Under D-02's (f)
            // the session refreshes the access token itself, so what decides
            // whether `claude` will work is the refresh token, a blanked file,
            // or a stale mount (F-12) — one report, shared with `attach`.
            let (line, _) = credential_report(&d, &name);
            println!("{line}");
        }

        Command::SwitchAgent { name, agent } => {
            let mut session = daemon::connect().await?;
            let resp = {
                let __req = session.req(proto::SwitchAgentRequest {
                    name: name.clone(),
                    agent: agent.id().to_string(),
                });
                session
                    .client()
                    .switch_agent(__req)
                    .await
                    .map_err(status_err)?
                    .into_inner()
            };
            let now_agent: Agent = resp.now.parse().unwrap_or(Agent::ClaudeCode);
            if resp.previous == resp.now {
                println!("project {name:?} already runs {}", agent_label(&resp.now));
            } else {
                println!(
                    "project {name:?}: {} -> {}",
                    agent_label(&resp.previous),
                    agent_label(&resp.now)
                );
                if !now_agent.portability_verified() {
                    eprintln!();
                    eprintln!(
                        "note: {} is IMPLEMENTED BUT UNVERIFIED — its session state may not",
                        agent_label(&resp.now)
                    );
                    eprintln!("      survive a stop/restart or an export/import. See F-84.");
                }
                // Be blunt about what this does NOT do. Silence here would let
                // someone switch agents expecting their history to follow.
                println!();
                println!(
                    "The existing conversation stays on the volume, but {} will not see it —",
                    agent_label(&resp.now)
                );
                println!("each agent stores its history in its own format, and Nemr runs one");
                println!("agent at a time per project. This switches which one; it does not");
                println!("migrate the conversation.");
                println!();
                println!("(Cross-agent migration is a planned, separate capability. When it");
                println!(" lands, this command will be its home.)");
            }
        }

        Command::Import {
            bundle_or_name,
            bundle,
            size,
        } => {
            // One argument is the bundle; two are name-then-bundle. Keeping the
            // old two-argument form working matters more than a tidier grammar:
            // it is in the README and in muscle memory.
            let (explicit_name, bundle_path) = match bundle {
                Some(path) => (Some(bundle_or_name), path),
                None => (None, std::path::PathBuf::from(bundle_or_name)),
            };
            let mut session = daemon::connect().await?;
            let resp = {
                let __req = session.req(proto::ImportRequest {
                    bundle_path: bundle_path.to_string_lossy().into_owned(),
                    name: explicit_name.unwrap_or_default(),
                    size: size.map(|s| s.to_string()).unwrap_or_default(),
                });
                session
                    .client()
                    .import(__req)
                    .await
                    .map_err(status_err)?
                    .into_inner()
            };
            let name = resp.name;
            println!(
                "imported {} into project {name:?} ({} members, {})",
                bundle_path.display(),
                resp.members,
                volume::human_bytes(resp.bytes)
            );
            println!("\nThe bundle carried no credential, and never does (D-02).");
            println!("Then: nemr start {name} && nemr attach {name} — and `/login` inside the session if it says no login yet.");
            // Suggested, never run (F-118): import works offline and
            // provisioning needs the network — the same seam as authentication
            // above. The same shape as every other next-step line this CLI
            // prints: say what is needed and the exact command, do nothing.
            if resp.declared_packages > 0 {
                println!(
                    "This session declares {} package(s) not in the base image. After starting: \
                     nemr provision {name}",
                    resp.declared_packages
                );
            }
        }

        Command::Export {
            name,
            output,
            include_build_artifacts,
        } => {
            let mut session = daemon::connect().await?;
            let destination =
                output.unwrap_or_else(|| std::path::PathBuf::from(format!("{name}.nemr")));

            let summary = {
                let __req = session.req(proto::ExportRequest {
                    name: name.clone(),
                    destination: destination.to_string_lossy().into_owned(),
                    include_build_artifacts,
                });
                session
                    .client()
                    .export(__req)
                    .await
                    .map_err(status_err)?
                    .into_inner()
            };

            println!("exported project {name:?} to {}", summary.path);
            println!(
                "  schema:    v{} (see docs/bundle-format.md)",
                summary.schema_version
            );
            println!(
                "  contents:  {} members, {} uncompressed -> {} on disk",
                summary.members,
                volume::human_bytes(summary.content_bytes),
                volume::human_bytes(summary.bundle_bytes)
            );

            // F-54: schema drift must be visible, not silent.
            if !summary.unrecognised_fields.is_empty() {
                eprintln!(
                    "\n[nemr] warning: {} unrecognised field(s) in .claude.json did NOT travel:",
                    summary.unrecognised_fields.len()
                );
                for field in &summary.unrecognised_fields {
                    eprintln!("         {field}");
                }
                eprintln!(
                    "       If any of these should travel, add them to the portable allowlist."
                );
            }

            // The credential exclusion is a security property (D-02), so state
            // it rather than leaving the user to infer it from a count.
            println!("\nCredentials were not included and never are (D-02).");
            println!("On the destination, authenticate on that host before attaching.");
        }

        Command::Attach { name } => {
            // Say it BEFORE Claude Code's own login prompt does the wrong thing.
            // A `/login` inside a session cannot rescue a spent refresh token or
            // a blanked file (the host must log in), and a stale mount (F-12)
            // needs a stop/start — none of which Claude Code's error names. The
            // facts come from the daemon's status, the same ones `nemr status`
            // prints, so the two never disagree. Non-fatal: the shell stays
            // useful. A routine access-token expiry is NOT warned about: the
            // writable mount lets the session refresh it (D-02 (f)).
            let mut session = daemon::connect().await?;
            let status_req = session.req(proto::StatusRequest { name: name.clone() });
            if let Ok(resp) = session.client().status(status_req).await {
                if let (_, Some(warning)) = credential_report(&resp.into_inner(), &name) {
                    eprintln!("{warning}");
                }
            }
            let code = attach_client(session.client().clone(), &name).await?;
            drop(session);
            // The session's exit code becomes ours, so scripts can branch on
            // what happened inside the container.
            if code != 0 {
                std::process::exit(code);
            }
        }

        Command::Port { action } => {
            let mut session = daemon::connect().await?;
            let (name, resp) = match action {
                PortAction::Add { name, port, expose } => {
                    let want_host_port = parsed_host_port(&port, expose);
                    let request = session.req(proto::PortAddRequest {
                        name: name.clone(),
                        port,
                        expose,
                    });
                    let r = session
                        .client()
                        .port_add(request)
                        .await
                        .map_err(status_err)?
                        .into_inner();
                    // Warn on the bind that actually resulted, and only once it
                    // has succeeded (F-98). Gating on the flag warned for adds
                    // that then failed, and stayed silent for the other way to
                    // opt out — writing a non-loopback address into the spec,
                    // which the parser documents as beating the flag.
                    if let Some(added) = r.ports.iter().find(|p| p.host_port == want_host_port) {
                        if !is_loopback(&added.host_ip) {
                            eprintln!(
                                "[nemr] {} is reachable by anything that can reach this machine, \
                                 not just you.",
                                added.host_ip
                            );
                        }
                    }
                    (name, r)
                }
                PortAction::Rm { name, host_port } => {
                    let request = session.req(proto::PortRemoveRequest {
                        name: name.clone(),
                        host_port: host_port as u32,
                    });
                    let r = session
                        .client()
                        .port_remove(request)
                        .await
                        .map_err(status_err)?
                        .into_inner();
                    (name, r)
                }
                PortAction::Ls { name } => {
                    let request = session.req(proto::PortListRequest { name: name.clone() });
                    let r = session
                        .client()
                        .port_list(request)
                        .await
                        .map_err(status_err)?
                        .into_inner();
                    (name, r)
                }
            };

            if resp.ports.is_empty() {
                println!("{name:?} forwards no ports.");
                println!("Forward one with: nemr port add {name} 8000");
                return Ok(());
            }
            println!("{:<22} {:<16} URL", "HOST", "CONTAINER PORT");
            for p in &resp.ports {
                let bind = format!("{}:{}", p.host_ip, p.host_port);
                let exposed = if p.host_ip == "0.0.0.0" {
                    "  (exposed)"
                } else {
                    ""
                };
                println!("{bind:<22} {:<16} {}{exposed}", p.container_port, p.url);
            }
        }

        Command::External(args) => {
            // `nemr <cmd> …` → exec `nemr-<cmd> …` from PATH, cargo/git style.
            //
            // exec(), not spawn-and-wait: the extension inherits our stdio and
            // TTY and its exit code is the process's own, with no wrapper in
            // between to garble signals or codes. This match arm touches no
            // daemon and no engine state — it is pure delegation.
            use std::os::unix::process::CommandExt;
            let name = args[0].to_string_lossy().into_owned();

            // A subcommand name never contains a path separator. Without this,
            // `Command::new` treats any name with a `/` as a PATH-free path —
            // so `nemr ../evil` would run `nemr-../evil` relative to the
            // current directory, executing a binary from a location PATH never
            // sanctioned (F-92). Refuse rather than resolve.
            if name.contains('/') {
                eprintln!("error: not a subcommand name: {name}");
                eprintln!("       (subcommand names cannot contain '/'. Extensions are");
                eprintln!("       found on PATH as `nemr-<name>`, never by path.)");
                std::process::exit(2);
            }
            let program = format!("nemr-{name}");
            let err = std::process::Command::new(&program).args(&args[1..]).exec();
            // exec only returns on failure.
            if err.kind() == std::io::ErrorKind::NotFound {
                eprintln!("error: no such subcommand: {name}");
                eprintln!("       (also looked for `{program}` on PATH — the form optional");
                eprintln!("       extensions install under. See `nemr --help` for built-ins.)");
            } else {
                eprintln!("error: could not run `{program}`: {err}");
            }
            std::process::exit(2);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `nemr add <dir>` derives a session name from the folder, and the derived
    /// name has to be one the engine accepts: `[a-z0-9][a-z0-9-]*`. Mapping
    /// every other character to `-` without trimming produced `-claude` for
    /// `.claude`, which the daemon then rejected — a default that cannot work.
    /// The page's panel derives the same way, so both offer the same name.
    #[test]
    fn a_folder_name_becomes_a_name_the_engine_accepts() {
        for (folder, expected) in [
            ("nemr-engine", "nemr-engine"),
            ("My Project", "my-project"),
            (".claude", "claude"),
            ("_scratch_", "scratch"),
            ("a..b", "a-b"),
            ("Ünicode", "nicode"),
        ] {
            let derived = name_from_basename(folder);
            assert_eq!(derived, expected, "deriving from {folder:?}");
            assert!(
                nemr_engine::engine::volume::validate_name(&derived).is_ok(),
                "{folder:?} derived {derived:?}, which the engine refuses"
            );
        }
        // Nothing usable is left: the caller must ask or require --name rather
        // than send an invalid one.
        assert_eq!(name_from_basename("..."), "");
        assert_eq!(name_from_basename("—"), "");
    }
}
