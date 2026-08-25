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
use crate::proto::{AuditEvent, WatchAuditRequest};
use crate::proto::{HandshakeRequest, PROTOCOL_VERSION};
use anyhow::{bail, Context, Result};
use std::time::Duration;
use tokio::net::UnixStream;
use tonic::transport::{Channel, Endpoint, Uri};
use tower::service_fn;

pub type Client = NemrClient<Channel>;

/// Set once at startup so the audit printer knows whether to show trace lines.
static VERBOSE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub fn set_verbose(v: bool) {
    VERBOSE.store(v, std::sync::atomic::Ordering::Relaxed);
}

/// A connected session: the client, plus a live audit stream whose events are
/// printed as the command runs. The audit printer stops when the session drops
/// (at the end of a command), so a command's elevation notes appear inline —
/// exactly what the pre-daemon CLI did, restored across the daemon boundary.
pub struct Session {
    client: Client,
    request_id: String,
    printer: Option<tokio::task::JoinHandle<()>>,
}

impl Session {
    /// The client for issuing RPCs.
    pub fn client(&mut self) -> &mut Client {
        &mut self.client
    }

    /// Wrap a message in a request carrying this session's id, so the daemon
    /// attributes the audit events it produces to this session's stream.
    pub fn req<T>(&self, msg: T) -> tonic::Request<T> {
        let mut r = tonic::Request::new(msg);
        if let Ok(v) = self.request_id.parse() {
            r.metadata_mut().insert("nemr-request-id", v);
        }
        r
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(p) = self.printer.take() {
            p.abort();
        }
    }
}

/// Connect to the daemon, autostarting it if needed, complete the version
/// handshake, and open this command's audit stream. Every command goes through
/// this — there is no direct path.
pub async fn connect() -> Result<Session> {
    let path = socket_path()?;

    let client = match try_connect(&path).await {
        Ok(client) => handshake(client).await?,
        Err(_) => {
            autostart(&path).await?;
            let client = try_connect(&path)
                .await
                .context("the daemon did not become reachable after autostart")?;
            handshake(client).await?
        }
    };

    open_session(client).await
}

/// A monotonic-ish request id: pid plus a per-process counter. Unique per
/// command within this CLI process, which is all the correlation needs.
fn next_request_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

async fn open_session(client: Client) -> Result<Session> {
    let request_id = next_request_id();

    // Open the audit stream on a clone of the channel (tonic clients are cheap
    // and share the channel), and wait for Ready before returning — so the
    // command the caller runs next cannot race the events it produces.
    let mut audit_client = client.clone();
    let mut stream = audit_client
        .watch_audit(WatchAuditRequest {
            request_id: request_id.clone(),
        })
        .await
        .context("failed to open the audit stream")?
        .into_inner();

    // Wait for the Ready sentinel (bounded, so a misbehaving daemon cannot hang
    // the CLI forever).
    let ready = tokio::time::timeout(Duration::from_secs(15), stream.message())
        .await
        .context("timed out waiting for the audit stream to become ready")?
        .context("audit stream error")?;
    match ready {
        Some(AuditEvent { ready: true, .. }) => {}
        _ => bail!("audit stream did not open with a ready event"),
    }

    // Print events as they arrive, for the life of the command.
    let printer = tokio::spawn(async move {
        let verbose = VERBOSE.load(std::sync::atomic::Ordering::Relaxed);
        while let Ok(Some(ev)) = stream.message().await {
            if ev.ready {
                continue;
            }
            if ev.privileged {
                // Always: a user must be able to tell a command elevated (NFR-04).
                eprintln!("{}", ev.message);
            } else if verbose {
                eprintln!("{}", ev.message);
            }
        }
    });

    Ok(Session {
        client,
        request_id,
        printer: Some(printer),
    })
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

    // Detach fully, but keep the daemon's output: it carries the VOL-03/NFR-04
    // audit trail (every privileged call, every mount), which must not vanish
    // just because the engine now runs in the daemon. Append to a durable log
    // under $XDG_STATE_HOME (or ~/.local/state), so an operator can reconstruct
    // what happened — the same guarantee the pre-daemon CLI gave inline.
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let log_dir = std::env::var_os("XDG_STATE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local/state"))
        })
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join("nemr");
    let _ = std::fs::create_dir_all(&log_dir);
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_dir.join("nemrd.log"))
        .ok();
    let mut cmd = Command::new(&nemrd);
    cmd.stdin(Stdio::null());
    match log {
        Some(f) => {
            let f2 = f
                .try_clone()
                .unwrap_or_else(|_| f.try_clone().expect("clone log"));
            cmd.stdout(Stdio::from(f)).stderr(Stdio::from(f2));
        }
        None => {
            cmd.stdout(Stdio::null()).stderr(Stdio::null());
        }
    }
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
    for _ in 0..150 {
        if try_connect(path).await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    bail!("nemrd was spawned but did not start listening within 5s")
}
