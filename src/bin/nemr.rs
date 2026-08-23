//! `nemr` — the CLI, and the sole user interface for Phase 1 (Section 1.3).
//!
//! Subcommands arrive per milestone: `create` at Milestone 4, start/attach/stop
//! at Milestone 5, list/delete at Milestone 6.

use anyhow::Result;
use clap::{Parser, Subcommand};

use nemr_engine::containerd::client::ContainerdClient;
use nemr_engine::containerd::containers::StopOutcome;
use nemr_engine::engine::agent::Agent;
use nemr_engine::engine::project;
use nemr_engine::engine::volume::{self, VolumeSize};

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
    List,

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
}

/// Parse `--size`, reusing the engine's own preset parsing so the CLI cannot
/// drift from what the engine and the privileged helper accept.
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

#[tokio::main]
async fn main() -> Result<()> {
    // Parse first: the subscriber's verbosity is now a flag, so it cannot be
    // installed before the flag is known. Nothing logs during parsing.
    let cli = Cli::parse();

    // Install the subscriber before anything else can log. --verbose and
    // NEMR_DEBUG=1 turn on the decision-point detail; NEMR_LOG takes a full
    // env-filter and overrides both.
    nemr_engine::observability::init(cli.verbose);

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

            let client = ContainerdClient::connect().await?;
            eprintln!(
                "[nemr] containerd: {} (namespace {})",
                client.socket_path().display(),
                client.namespace()
            );

            let project = project::create(&client, &name, size, agent).await?;

            println!("created project {:?}", project.name);
            println!("  agent:     {}", project.agent.label());
            if !project.agent.portability_verified() {
                // At the point of selection, not only in docs someone may not
                // read: "implemented the same way" must not quietly become
                // "works" (E-15/F-84).
                eprintln!();
                eprintln!(
                    "  note: {} support is IMPLEMENTED BUT UNVERIFIED.",
                    project.agent.label()
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
            let client = ContainerdClient::connect().await?;
            let pid = project::start(&client, &name).await?;
            println!("started project {name:?} (supervisor pid {pid})");
            println!("  attach with: nemr attach {name}");
        }

        Command::Stop { name } => {
            let client = ContainerdClient::connect().await?;
            let outcome = project::stop(&client, &name).await?;
            println!("stopped project {name:?} ({outcome})");

            // A container that had to be killed got no chance to shut down
            // cleanly. Not an error, but the user should not have to guess
            // which of the two happened (PROC-06).
            if outcome == StopOutcome::Killed {
                eprintln!(
                    "[nemr] warning: the container ignored SIGTERM and was killed after the \n\
                     grace period. Its processes were given no opportunity to flush state."
                );
            }
        }

        Command::List => {
            let client = ContainerdClient::connect().await?;
            let projects = project::list(&client).await?;

            if projects.is_empty() {
                println!("no projects. Create one with: nemr create <name> --size 2GB");
                return Ok(());
            }

            println!(
                "{:<18} {:<9} {:<18} {:<8} VOLUME",
                "NAME", "STATUS", "USED", "QUOTA"
            );
            for p in &projects {
                let status = if p.running { "running" } else { "stopped" };
                let used = match p.usage {
                    Some(u) => format!("{} ({:.0}%)", volume::human_bytes(u.used), u.percent()),
                    None => "unmounted".to_string(),
                };
                println!(
                    "{:<18} {:<9} {:<18} {:<8} {}",
                    p.name, status, used, p.quota, p.volume_path
                );
            }

            // F-77: a listing that shows only container-backed projects is true
            // and misleading when orphaned images are filling the disk.
            let untracked = project::untracked_volumes(&client).await?;
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
            let client = ContainerdClient::connect().await?;

            // AC-6.2: deletion is destructive and irreversible — the volume and
            // everything written to it goes. Confirm unless explicitly waived.
            if !yes {
                let projects = project::list(&client).await?;
                let target = projects.iter().find(|p| p.name == name);
                match target {
                    Some(p) => {
                        let used = p
                            .usage
                            .map(|u| volume::human_bytes(u.used))
                            .unwrap_or_else(|| "unknown".into());
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

            project::delete(&client, &name).await?;
            println!("deleted project {name:?}");
        }

        Command::Reconcile => {
            let client = ContainerdClient::connect().await?;
            let report = project::reconcile_orphans(&client).await?;
            if report.is_empty() {
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
            let client = ContainerdClient::connect().await?;
            let d = project::status(&client, &name).await?;

            println!("{}", d.name);
            println!("  agent:        {}", d.agent.label());
            println!(
                "  state:        {}",
                if d.running { "running" } else { "stopped" }
            );
            println!("  container:    {}", d.container_id);

            let usage = match &d.usage {
                Some(u) => format!(
                    "{} of {} ({:.0}%)",
                    volume::human_bytes(u.used),
                    d.quota,
                    u.percent()
                ),
                None => format!("unmounted (quota {})", d.quota),
            };
            println!("  volume:       {}", d.mount_point.display());
            println!("  usage:        {usage}");
            println!(
                "  image file:   {} ({})",
                d.image_file.display(),
                if d.image_present {
                    "present"
                } else {
                    "MISSING"
                }
            );
            println!(
                "  loop device:  {}",
                d.loop_device
                    .map(|n| format!("/dev/loop{n}"))
                    .unwrap_or_else(|| "none".into())
            );

            // F-28: the question that cost the most time to answer by hand.
            match d.mount_is_correct() {
                None => println!("  mount check:  n/a (not mounted)"),
                Some(true) => println!("  mount check:  ok (backed by this project's image)"),
                Some(false) => println!(
                    "  mount check:  WRONG VOLUME — mounted filesystem is backed by {}\n\
                     \x20               Run `nemr reconcile`, then start again.",
                    d.mounted_image
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "something that is not a loop device".into())
                ),
            }

            println!("  base image:   {}", d.base_image);
            println!(
                "  base digest:  {}",
                d.base_image_digest
                    .as_deref()
                    .unwrap_or("NOT PRESENT on this host")
            );

            // The expired-credential failure took three steps to identify; this
            // is the line that would have made it one.
            match (&d.credential, d.credential_modified) {
                (Some(path), modified) => {
                    let age = modified
                        .and_then(|m| m.elapsed().ok())
                        .map(|d| format!(", last written {} days ago", d.as_secs() / 86_400))
                        .unwrap_or_default();
                    println!("  credential:   present at {}{age}", path.display());
                }
                (None, _) => println!(
                    "  credential:   ABSENT — `nemr start` will fail (AUTH-03).\n\
                     \x20               Authenticate on this host by running `claude`."
                ),
            }
        }

        Command::SwitchAgent { name, agent } => {
            let client = ContainerdClient::connect().await?;
            let (previous, now) = project::set_agent(&client, &name, agent).await?;
            if previous == now {
                println!("project {name:?} already runs {}", now.label());
            } else {
                println!("project {name:?}: {} -> {}", previous.label(), now.label());
                if !now.portability_verified() {
                    eprintln!();
                    eprintln!(
                        "note: {} is IMPLEMENTED BUT UNVERIFIED — its session state may not",
                        now.label()
                    );
                    eprintln!("      survive a stop/restart or an export/import. See F-84.");
                }
                // Be blunt about what this does NOT do. Silence here would let
                // someone switch agents expecting their history to follow.
                println!();
                println!(
                    "The existing conversation stays on the volume, but {} will not see it —",
                    now.label()
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
            let client = ContainerdClient::connect().await?;
            let (name, summary) =
                project::import_creating(&client, &bundle_path, explicit_name.as_deref(), size)
                    .await?;
            println!(
                "imported {} into project {name:?} ({} members, {})",
                bundle_path.display(),
                summary.members,
                volume::human_bytes(summary.bytes)
            );
            println!("\nThe bundle carried no credential, and never does (D-02).");
            println!("Authenticate on this host, then: nemr start {name} && nemr attach {name}");
        }

        Command::Export {
            name,
            output,
            include_build_artifacts,
        } => {
            let client = ContainerdClient::connect().await?;
            let destination =
                output.unwrap_or_else(|| std::path::PathBuf::from(format!("{name}.nemr")));
            let policy = nemr_engine::bundle::policy::Policy {
                include_build_artifacts,
            };

            let summary = project::export(&client, &name, &destination, policy).await?;
            let manifest = &summary.manifest;

            println!("exported project {name:?} to {}", summary.path.display());
            println!(
                "  schema:    v{} (see docs/bundle-format.md)",
                manifest.schema_version
            );
            println!(
                "  contents:  {} members, {} uncompressed -> {} on disk",
                manifest.members.len(),
                volume::human_bytes(manifest.project.content_bytes),
                volume::human_bytes(summary.bundle_bytes)
            );
            println!(
                "  base image: {} ({})",
                manifest.base_image.reference, manifest.base_image.digest
            );
            println!("  excluded:  {} entries", manifest.excluded.len());

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
            let client = ContainerdClient::connect().await?;
            let code = project::attach(&client, &name).await?;
            // The session's exit code becomes ours, so scripts can branch on
            // what happened inside the container.
            if code != 0 {
                std::process::exit(code as i32);
            }
        }
    }

    Ok(())
}
