//! CLI side of the daemon connection (E-09): connect to the nemrd socket,
//! autostart the daemon if none is listening, and negotiate the protocol
//! version before any command runs.
//!
//! Lifecycle: autostart-on-demand. If no daemon answers, the CLI spawns one and
//! waits for it — the gpg-agent/ssh-agent pattern, so a fresh install works with
//! no setup step. A systemd user unit ships too (deploy/systemd/user), for those
//! who want the daemon managed, but nothing depends on it. Because the daemon
//! holds no session state — the container's PID 1 is the supervisor — a daemon
//! crash does not kill running sessions: the next command autostarts a fresh
//! daemon that reconnects to the same containerd state.

use crate::daemon::socket::socket_path;
use crate::proto::nemr_client::NemrClient;
use crate::proto::{HandshakeRequest, PROTOCOL_VERSION};
use anyhow::{bail, Context, Result};
use std::time::Duration;
use tokio::net::UnixStream;
use tonic::transport::{Channel, Endpoint, Uri};
use tower::service_fn;

pub type Client = NemrClient<Channel>;

/// Connect to the daemon, autostarting it if needed, and complete the version
/// handshake. Every command goes through this — there is no direct path.
pub async fn connect() -> Result<Client> {
    let path = socket_path()?;

    // Try an existing daemon first; only autostart if nothing answers.
    if let Ok(client) = try_connect(&path).await {
        return handshake(client).await;
    }

    autostart(&path).await?;
    let client = try_connect(&path)
        .await
        .context("the daemon did not become reachable after autostart")?;
    handshake(client).await
}

async fn try_connect(path: &std::path::Path) -> Result<Client> {
    let path = path.to_path_buf();
    // The URI is ignored by the UDS connector but tonic requires a valid one.
    let channel = Endpoint::try_from("http://[::]:0")?
        .connect_timeout(Duration::from_secs(5))
        .connect_with_connector(service_fn(move |_: Uri| {
            let path = path.clone();
            async move {
                let stream = UnixStream::connect(path).await?;
                Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
            }
        }))
        .await
        .context("failed to connect to the nemrd socket")?;
    Ok(NemrClient::new(channel))
}

async fn handshake(mut client: Client) -> Result<Client> {
    let resp = client
        .handshake(HandshakeRequest {
            protocol_version: PROTOCOL_VERSION,
            client_build: env!("CARGO_PKG_VERSION").to_string(),
        })
        .await
        .map_err(|status| {
            // A version mismatch is a FailedPrecondition with an actionable
            // message; surface it as-is rather than a generic RPC error.
            anyhow::anyhow!("{}", status.message())
        })?;
    let resp = resp.into_inner();
    if resp.protocol_version != PROTOCOL_VERSION {
        bail!(
            "daemon protocol v{} does not match this CLI's v{}. Reinstall both from the same \
             build: ./scripts/install_engine.sh",
            resp.protocol_version,
            PROTOCOL_VERSION
        );
    }
    Ok(client)
}

/// Spawn a detached daemon and wait for its socket to answer.
async fn autostart(path: &std::path::Path) -> Result<()> {
    // The daemon binary sits beside this one.
    let exe = std::env::current_exe().context("cannot locate the running executable")?;
    let nemrd = exe.with_file_name("nemrd");
    if !nemrd.exists() {
        bail!(
            "no nemrd daemon is running and the daemon binary was not found at {}. \
             Reinstall: ./scripts/install_engine.sh",
            nemrd.display()
        );
    }

    // Detach fully: new session, IO to /dev/null, so it outlives this CLI.
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(&nemrd);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(sock) = std::env::var_os("NEMR_DAEMON_SOCKET") {
        cmd.env("NEMR_DAEMON_SOCKET", sock);
    }
    unsafe {
        cmd.pre_exec(|| {
            // setsid so the daemon is not in the CLI's process group and does
            // not die when the CLI's terminal closes.
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn().context("failed to spawn nemrd")?;

    // Wait for the socket to accept a connection (bounded).
    for _ in 0..50 {
        if try_connect(path).await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    bail!("nemrd was spawned but did not start listening within 5s")
}
