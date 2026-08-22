//! `nemr` — the CLI, and the sole user interface for Phase 1 (Section 1.3).
//!
//! Subcommands arrive per milestone: `create` at Milestone 4, start/attach/stop
//! at Milestone 5, list/delete at Milestone 6.

use anyhow::Result;
use clap::{Parser, Subcommand};

use nemr_engine::containerd::client::ContainerdClient;
use nemr_engine::containerd::containers::StopOutcome;
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
}

#[derive(Subcommand)]
enum Command {
    /// Create a project: a quota-bounded volume plus a ready-to-start container.
    Create {
        /// Project name (lowercase letters, digits and '-').
        name: String,

        /// Storage quota, fixed at creation time.
        #[arg(long, default_value = "2GB", value_parser = parse_size)]
        size: VolumeSize,
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

    /// Import a bundle into an existing, stopped project.
    ///
    /// Works standalone against a local file: no account, no network (E-11).
    Import {
        /// Destination project. Create it first with the quota you want.
        name: String,
        /// Bundle to read.
        bundle: std::path::PathBuf,
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
fn parse_size(input: &str) -> Result<VolumeSize, String> {
    input.parse::<VolumeSize>().map_err(|e| e.to_string())
}

#[tokio::main]
async fn main() -> Result<()> {
    // Install the subscriber before anything can log. NEMR_DEBUG=1 turns on the
    // decision-point detail; NEMR_LOG takes a full env-filter.
    nemr_engine::observability::init();

    let cli = Cli::parse();

    match cli.command {
        Command::Create { name, size } => {
            let client = ContainerdClient::connect().await?;
            eprintln!(
                "[nemr] containerd: {} (namespace {})",
                client.socket_path().display(),
                client.namespace()
            );

            let project = project::create(&client, &name, size).await?;

            println!("created project {:?}", project.name);
            println!("  container: {}", project.container_id);
            println!("  volume:    {} ({})", project.volume_path, project.size);
            println!("  status:    stopped (ready to start)");
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

        Command::Import { name, bundle } => {
            let client = ContainerdClient::connect().await?;
            let summary = project::import(&client, &name, &bundle).await?;
            println!(
                "imported {} into project {name:?} ({} members, {})",
                bundle.display(),
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
