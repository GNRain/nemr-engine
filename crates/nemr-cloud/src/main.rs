//! The Nemr sync client (WP-K) — the commercial half's CLI.
//!
//! One binary, many names. It is installed as `nemr-cloud` plus symlinks
//! (`nemr-login`, `nemr-logout`, `nemr-register`, `nemr-sessions`, `nemr-push`,
//! `nemr-pull`, `nemr-release`); invoked through a symlink it dispatches on its
//! own argv[0], which is what makes `nemr login` work end to end: the open CLI
//! execs `nemr-login`, which is this binary wearing that name.
//!
//! The flow it exists for: log in once, work in a session, `nemr push` when you
//! stop; on another machine `nemr pull`, attach, continue. The server stores
//! ciphertext it cannot read (E-16): every bundle is encrypted here, client
//! side, under a master key the server never sees.

use anyhow::Result;
use clap::{Parser, Subcommand};

mod api;
mod commands;
mod core;
mod daemon;
mod engine_cli;
mod keys;
mod serve;
mod server;
mod state;

#[derive(Parser)]
#[command(name = "nemr-cloud", about = "Nemr sync client (commercial half)")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create an account. Generates the master key and the recovery code, and
    /// requires the code re-typed before the account is usable (E-16).
    Register {
        /// Server URL. Also NEMR_SERVER_URL; stored at login.
        #[arg(long)]
        server: Option<String>,
        /// Account email. Also NEMR_CLOUD_EMAIL; prompted if absent.
        #[arg(long)]
        email: Option<String>,
    },

    /// Log in: fetch and unwrap nothing — store the token and the key envelope
    /// so data commands can derive the master key from the password locally.
    Login {
        #[arg(long)]
        server: Option<String>,
        #[arg(long)]
        email: Option<String>,
    },

    /// Log out: revoke the token server-side and remove local state.
    Logout,

    /// List sessions — the server-side index alongside local projects, marked
    /// so it is obvious which is which.
    Sessions,

    /// Encrypt and upload a project's bundle. Requires the lease; a machine
    /// that lost the lease is refused — by this client and by the server.
    Push {
        name: String,
        /// Release the lease after a successful upload (a clean "stop").
        #[arg(long)]
        release: bool,
        /// Take the lease from its current holder if another machine has it.
        #[arg(long)]
        take_over: bool,
    },

    /// Download, decrypt and import a session from the server.
    Pull {
        name: String,
        /// Take the lease from its current holder if another machine has it.
        #[arg(long)]
        take_over: bool,
    },

    /// Release the session's lease and stop the heartbeat holder.
    Release { name: String },

    /// Start the UI: a loopback port that exists only while this runs, with
    /// a single-use launch token in the URL. Prints the URL; opens it unless
    /// --no-open. Ctrl-C stops the UI and closes the port.
    Ui {
        /// Port to bind on 127.0.0.1 (default: an ephemeral one).
        #[arg(long)]
        port: Option<u16>,
        /// Print the URL only; do not try to open a browser.
        #[arg(long)]
        no_open: bool,
    },

    /// Run the sync server: one command instead of a screen of environment
    /// variables. SELF-HOSTING AND DEVELOPMENT ONLY — the hosted product does
    /// not need this, and a normal user never starts a server.
    ///
    /// Settings come from sync.env and the environment (E-19). It does not
    /// install or start Postgres: it checks that one answers and refuses,
    /// naming the connection it tried.
    Server {
        #[command(subcommand)]
        action: ServerAction,
    },

    /// INTERNAL: the detached lease-heartbeat holder. Spawned by push/pull;
    /// renews until it fails, then marks the lease lost and refuses to
    /// continue. Not for direct use.
    #[command(hide = true, name = "__hold")]
    Hold {
        name: String,
        #[arg(long)]
        holder: String,
        #[arg(long)]
        fence: i64,
        /// Milliseconds between heartbeats.
        #[arg(long)]
        interval_ms: u64,
    },
}

#[derive(Subcommand)]
enum ServerAction {
    /// Start it in the FOREGROUND: this process becomes the server, so Ctrl-C
    /// stops it and nothing outlives the window you ran it in.
    Start,
    /// Stop the server this command started (SIGTERM; never SIGKILL).
    Stop,
    /// Is it running, on what address, on which storage — and do Postgres and
    /// that storage answer right now?
    Status,
}

fn main() -> Result<()> {
    // argv[0] dispatch: invoked as `nemr-login` etc. (via the open CLI's
    // external-subcommand exec), rewrite to the matching subcommand so one
    // binary serves every name.
    let mut args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    if let Some(prog) = args.first().cloned() {
        let base = std::path::Path::new(&prog)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if let Some(sub) = base.strip_prefix("nemr-") {
            if sub != "cloud" {
                args.insert(1, sub.into());
            }
        }
    }
    let cli = Cli::parse_from(args);

    match cli.command {
        Command::Register { server, email } => commands::register(server, email),
        Command::Login { server, email } => commands::login(server, email),
        Command::Logout => commands::logout(),
        Command::Sessions => commands::sessions(),
        Command::Push {
            name,
            release,
            take_over,
        } => commands::push(&name, release, take_over),
        Command::Pull { name, take_over } => commands::pull(&name, take_over),
        Command::Release { name } => commands::release(&name),
        Command::Ui { port, no_open } => serve::run(port, !no_open),
        Command::Server { action } => match action {
            ServerAction::Start => server::start(),
            ServerAction::Stop => server::stop(),
            ServerAction::Status => server::status(),
        },
        Command::Hold {
            name,
            holder,
            fence,
            interval_ms,
        } => commands::hold(&name, &holder, fence, interval_ms),
    }
}
