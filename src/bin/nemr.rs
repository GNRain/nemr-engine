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

/// A name suggested from the current directory — the common case is that the
/// project you want is named after where you are.
fn name_from_cwd() -> Option<String> {
    let raw = std::env::current_dir().ok()?;
    let base = raw.file_name()?.to_string_lossy().to_ascii_lowercase();
    // Only offer it if it is already a valid project name; never silently
    // mangle a directory name into something that only half resembles it.
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches('-').to_string();
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

fn resolve_size(provided: Option<VolumeSize>, interactive: bool) -> Result<VolumeSize> {
    let default = VolumeSize::default_size();
    match decide(provided, interactive, Some(default), "--size") {
        Resolution::Provided(v) | Resolution::UseDefault(v) => Ok(v),
        Resolution::MustFail { required_flag } => {
            anyhow::bail!("{required_flag} is required when stdin is not a terminal")
        }
        Resolution::Prompt { default } => {
            let options = VolumeSize::all();
            let start = options
                .iter()
                .position(|s| Some(*s) == default)
                .unwrap_or(0);
            let labels: Vec<String> = options.iter().map(|s| s.to_string()).collect();
            let choice = dialoguer::Select::new()
                .with_prompt("Storage size (fixed for the life of the project)")
                .items(&labels)
                .default(start)
                .interact()?;
            Ok(options[choice])
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

#[tokio::main]
async fn main() -> Result<()> {
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
            let name = resolve_name(name, interactive)?;
            let size = resolve_size(size, interactive)?;
            let agent = resolve_agent(agent, interactive)?;

            let mut session = daemon::connect().await?;
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

            println!("created project {:?}", project.name);
            println!("  agent:     {}", agent_label(&project.agent));
            if !project_agent.portability_verified() {
                // At the point of selection, not only in docs someone may not
                // read: "implemented the same way" must not quietly become
                // "works" (E-15/F-84).
                eprintln!();
                eprintln!(
                    "  note: {} support is IMPLEMENTED BUT UNVERIFIED.",
                    agent_label(&project.agent)
                );
                eprintln!("        Where it stores its conversation has not been measured, so a");
                eprintln!(
                    "        stop/restart or an export/import may silently lose it. See F-84."
                );
            }
            println!("  container: {}", project.container_id);
            println!("  volume:    {} ({})", project.volume_path, project.size);
            println!("  status:    stopped (ready to start)");
            println!("  next:      nemr start {}", project.name);
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
            println!("started project {name:?} (supervisor pid {pid})");
            println!("  attach with: nemr attach {name}");
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
                "graceful" => println!("stopped project {name:?} (terminated gracefully)"),
                "no_task" => println!("project {name:?} was not running"),
                "killed" => {
                    println!("stopped project {name:?} (ignored SIGTERM; killed)");
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

            if projects.is_empty() && resp.untracked_volumes.is_empty() {
                println!("no projects. Create one with: nemr create <name> --size 2GB");
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
                    println!(
                        "{:<18} {:<9} {:<18} {:<8} {}",
                        p.name, status, used, p.quota, p.volume_path
                    );
                }
            }

            // F-77: a listing that shows only container-backed projects is true
            // and misleading when orphaned images are filling the disk.
            let untracked = resp.untracked_volumes;
            if !untracked.is_empty() {
                println!();
                println!(
                    "{} volume image(s) belong to no project and are using disk:",
                    untracked.len()
                );
                for name in untracked.iter().take(10) {
                    println!("  {name}");
                }
                if untracked.len() > 10 {
                    println!("  ... and {} more", untracked.len() - 10);
                }
                println!("Reclaim them with: nemr reconcile");
            }
        }

        Command::Delete { name, yes } => {
            let mut session = daemon::connect().await?;

            // AC-6.2: deletion is destructive and irreversible — the volume and
            // everything written to it goes. Confirm unless explicitly waived.
            if !yes {
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
                        eprintln!("About to delete project {name:?}:");
                        eprintln!("  container: {}", p.container_id);
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
                        eprintln!("This permanently destroys the volume and everything in it.");
                    }
                    None => eprintln!("About to delete project {name:?} (details unavailable)."),
                }
                eprint!("Type the project name to confirm: ");
                std::io::Write::flush(&mut std::io::stderr())?;

                let mut answer = String::new();
                std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut answer)?;
                if answer.trim() != name {
                    eprintln!("Cancelled: input did not match {name:?}. Nothing was deleted.");
                    std::process::exit(1);
                }
            }

            {
                let __req = session.req(proto::DeleteRequest { name: name.clone() });
                session.client().delete(__req).await.map_err(status_err)?
            };
            println!("deleted project {name:?}");
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
            {
                println!("nothing to reconcile: no orphaned mounts, loop devices or snapshots");
            } else {
                for name in &report.released {
                    println!("released orphan volume {name:?} (mount + loop device)");
                }
                for name in &report.not_released {
                    println!(
                        "FAILED to fully release {name:?}: the helper reported success but the \
                         host still shows it mounted or loop-attached. An attached loop device \
                         holds its image open, so that disk space is NOT reclaimed.\n  \
                         Check: losetup -a | grep {name}"
                    );
                }
                for key in &report.snapshots_removed {
                    println!("removed orphan snapshot {key:?}");
                }
                for name in &report.orphan_backing_files {
                    println!(
                        "orphan backing file for {name:?}: mount/loop released, but the file was \
                         KEPT — it may hold data. Remove it deliberately if you are sure."
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

            println!("{}", d.name);
            println!("  agent:        {}", agent_label(&d.agent));
            println!(
                "  state:        {}",
                if d.running { "running" } else { "stopped" }
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
                    "present"
                } else {
                    "MISSING"
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
                1 => println!("  mount check:  ok (backed by this project's image)"),
                _ => println!(
                    "  mount check:  WRONG VOLUME — mounted filesystem is backed by {}\n\
                     \x20               Run `nemr reconcile`, then start again.",
                    if d.mounted_image.is_empty() {
                        "something that is not a loop device".to_string()
                    } else {
                        d.mounted_image.clone()
                    }
                ),
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
            // is the line that would have made it one.
            if d.credential_path.is_empty() {
                println!(
                    "  credential:   ABSENT — `nemr start` will fail (AUTH-03).\n\
                     \x20               Authenticate on this host by running `claude`."
                );
            } else {
                let age = if d.credential_modified_secs > 0 {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0);
                    let days = (now - d.credential_modified_secs).max(0) / 86_400;
                    format!(", last written {days} days ago")
                } else {
                    String::new()
                };
                println!("  credential:   present at {}{age}", d.credential_path);
            }
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
            println!("Authenticate on this host, then: nemr start {name} && nemr attach {name}");
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
            let mut session = daemon::connect().await?;
            let code = attach_client(session.client().clone(), &name).await?;
            drop(session);
            // The session's exit code becomes ours, so scripts can branch on
            // what happened inside the container.
            if code != 0 {
                std::process::exit(code);
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
