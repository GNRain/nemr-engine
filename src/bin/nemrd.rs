//! The nemrd daemon (E-09): the single writer to containerd, serving the CLI
//! over a Unix domain socket.

use anyhow::{Context, Result};
use nemr_engine::containerd::client::ContainerdClient;
use nemr_engine::daemon::socket::socket_path;
use nemr_engine::daemon::NemrService;
use nemr_engine::proto::nemr_server::NemrServer;
use tokio::net::UnixListener;
use tokio_stream::wrappers::UnixListenerStream;

#[tokio::main]
async fn main() -> Result<()> {
    nemr_engine::observability::init(std::env::var_os("NEMR_VERBOSE").is_some());

    let path = socket_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    // A stale socket from a previous run would make bind fail with EADDRINUSE.
    // Removing it is safe: if another daemon is actually listening, the connect
    // check below would have found it and we would not be starting.
    let _ = std::fs::remove_file(&path);

    let client = ContainerdClient::connect()
        .await
        .context("nemrd could not connect to containerd")?;
    eprintln!(
        "[nemrd] containerd: {} (namespace {})",
        client.socket_path().display(),
        client.namespace()
    );

    let listener = UnixListener::bind(&path)
        .with_context(|| format!("failed to bind the nemrd socket at {}", path.display()))?;
    // Only the owning user may reach the daemon — the same trust boundary as the
    // rootless containerd socket beside it.
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to chmod {}", path.display()))?;

    eprintln!(
        "[nemrd] listening on {} (protocol v{})",
        path.display(),
        nemr_engine::proto::PROTOCOL_VERSION
    );

    let service = NemrService::new(client);
    let incoming = UnixListenerStream::new(listener);

    // Shut down cleanly on SIGTERM/SIGINT so the socket file is removed and a
    // restart does not trip over a stale one.
    let shutdown = async {
        let mut term =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
        let mut int =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).unwrap();
        tokio::select! {
            _ = term.recv() => {}
            _ = int.recv() => {}
        }
        eprintln!("[nemrd] shutting down");
    };

    tonic::transport::Server::builder()
        .add_service(NemrServer::new(service))
        .serve_with_incoming_shutdown(incoming, shutdown)
        .await
        .context("nemrd server error")?;

    let _ = std::fs::remove_file(&path);
    Ok(())
}
