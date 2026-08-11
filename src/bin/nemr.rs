//! `nemr` — the CLI, and the sole user interface for Phase 1 (Section 1.3).
//!
//! Subcommands arrive per milestone: `create` at Milestone 4, start/attach/stop
//! at Milestone 5, list/delete at Milestone 6.

use anyhow::Result;
use clap::{Parser, Subcommand};

use nemr_engine::containerd::client::ContainerdClient;
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
}

/// Parse `--size`, reusing the engine's own preset parsing so the CLI cannot
/// drift from what the engine and the privileged helper accept.
fn parse_size(input: &str) -> Result<VolumeSize, String> {
    input.parse::<VolumeSize>().map_err(|e| e.to_string())
}

#[tokio::main]
async fn main() -> Result<()> {
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
            project::stop(&client, &name).await?;
            println!("stopped project {name:?}");
        }

        Command::List => {
            let client = ContainerdClient::connect().await?;
            let projects = project::list(&client).await?;

            if projects.is_empty() {
                println!("no projects. Create one with: nemr create <name> --size 2GB");
                return Ok(());
            }

            println!("{:<18} {:<9} {:<18} {:<8} {}", "NAME", "STATUS", "USED", "QUOTA", "VOLUME");
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
                        eprintln!("  volume:    {} ({} used of {})", p.volume_path, used, p.quota);
                        eprintln!("  status:    {}", if p.running { "running (will be stopped)" } else { "stopped" });
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
