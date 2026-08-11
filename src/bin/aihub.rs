//! `aihub` — the CLI, and the sole user interface for Phase 1 (Section 1.3).
//!
//! Subcommands arrive per milestone: `create` at Milestone 4, start/attach/stop
//! at Milestone 5, list/delete at Milestone 6.

use anyhow::Result;
use clap::{Parser, Subcommand};

use ai_hub_engine::containerd::client::ContainerdClient;
use ai_hub_engine::engine::project;
use ai_hub_engine::engine::volume::VolumeSize;

#[derive(Parser)]
#[command(
    name = "aihub",
    about = "Isolated, quota-bounded Claude Code environments",
    long_about = "AI Hub engine — provisions isolated, resource-bounded Claude Code \
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
                "[aihub] containerd: {} (namespace {})",
                client.socket_path().display(),
                client.namespace()
            );

            let project = project::create(&client, &name, size).await?;

            println!("created project {:?}", project.name);
            println!("  container: {}", project.container_id);
            println!("  volume:    {} ({})", project.volume_path, project.size);
            println!("  status:    stopped (ready to start)");
        }
    }

    Ok(())
}
