//! The nemrd daemon (E-09): the single writer to containerd, serving the CLI
//! over a Unix domain socket.

use anyhow::{Context, Result};
use nemr_engine::containerd::client::ContainerdClient;
use nemr_engine::daemon::audit;
use nemr_engine::daemon::socket::socket_path;
use nemr_engine::daemon::NemrService;
use nemr_engine::proto::nemr_server::NemrServer;
use tokio::net::UnixListener;
use tokio_stream::wrappers::UnixListenerStream;

fn main() -> Result<()> {
    // `nemrd __rebind …` is the single-threaded child that re-binds the host's
    // current credential into a running session (F-12). It must run before any
    // runtime thread exists: joining a user namespace refuses a multi-threaded
    // process.
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("__rebind") {
        return nemr_engine::engine::credential_bind::rebind_main(&args[2..]);
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building the daemon runtime")?
        .block_on(daemon_main())
}

async fn daemon_main() -> Result<()> {
    let audit_registry = audit::new_registry();
    nemr_engine::observability::init_daemon(
        std::env::var_os("NEMR_VERBOSE").is_some(),
        audit_registry.clone(),
    );

    let path = socket_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    // Refuse to displace a daemon that is already answering.
    //
    // The CLI only autostarts when its own connect fails, but two CLIs can race
    // that check, and anything else may spawn a daemon directly. Every such
    // start used to steal the socket and leave the previous daemon running,
    // unreachable — six live nemrd processes were observed on one machine, all
    // but one orphaned, which contradicts E-09's single-writer claim in exactly
    // the way that makes a failure impossible to attribute.
    if tokio::time::timeout(
        std::time::Duration::from_secs(2),
        tokio::net::UnixStream::connect(&path),
    )
    .await
    .is_ok_and(|r| r.is_ok())
    {
        eprintln!(
            "[nemrd {}] a daemon is already listening on {}; exiting rather than \
             displacing it.",
            std::process::id(),
            path.display()
        );
        return Ok(());
    }

    // Connect to containerd BEFORE touching the socket path.
    //
    // This used to unlink the socket first, justified by "if another daemon is
    // actually listening, the connect check below would have found it" — but
    // that connect check lives in the CLI, in a different process. So a daemon
    // that could not reach containerd deleted a working daemon's socket and
    // exited, leaving every CLI on the machine with nothing to talk to; and
    // even on the happy path the socket was absent for the whole containerd
    // connect (measured 36ms idle, over a second under load), during which
    // every CLI got ENOENT and autostarted yet another daemon, which unlinked
    // and re-opened the window in turn.
    let client = ContainerdClient::connect()
        .await
        .context("nemrd could not connect to containerd")?;
    eprintln!(
        "[nemrd {}] containerd: {} (namespace {})",
        std::process::id(),
        client.socket_path().display(),
        client.namespace()
    );

    // Bind beside the real path, narrow the permissions, then rename into
    // place. `rename(2)` is atomic within a directory, so the well-known path
    // is never absent and never briefly world-reachable: it points at the old
    // daemon until the instant it points at this one. Binding directly created
    // the socket at 0777 & ~umask and only then chmod'ed it to 0600, leaving a
    // window in which any local user could reach the daemon that this file's
    // own header calls the trust boundary.
    let staged = path.with_extension(format!("sock.{}", std::process::id()));
    let _ = std::fs::remove_file(&staged);
    let listener = UnixListener::bind(&staged)
        .with_context(|| format!("failed to bind the nemrd socket at {}", staged.display()))?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to chmod {}", staged.display()))?;
    std::fs::rename(&staged, &path).with_context(|| {
        format!(
            "failed to move the nemrd socket into place at {}",
            path.display()
        )
    })?;

    // Which socket file is OURS, so shutdown does not remove a successor's.
    let ours = std::fs::metadata(&path).ok().map(|m| {
        use std::os::unix::fs::MetadataExt;
        (m.dev(), m.ino())
    });

    eprintln!(
        "[nemrd {}] listening on {} (protocol v{})",
        std::process::id(),
        path.display(),
        nemr_engine::proto::PROTOCOL_VERSION
    );

    let last_credential_write = nemr_engine::daemon::credential_watch::new_last_write();
    let service = NemrService::new(client, audit_registry, last_credential_write.clone());
    // D-02 (f): observe every rewrite of the host credential, and re-bind
    // running sessions when the host replaces it (F-12). Absent credential:
    // nothing to watch yet; the watcher starts with the next daemon.
    match nemr_engine::auth::host_credentials_path() {
        Ok(cred) if cred.exists() => nemr_engine::daemon::credential_watch::spawn(
            service.client(),
            cred,
            last_credential_write,
        ),
        _ => eprintln!("[nemrd] no host credential to watch yet"),
    }
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
        // Wrap every RPC in a request-id span so the audit Layer can attribute
        // the engine events it produces to the right client (E-09).
        .layer(audit::RequestSpanLayer)
        .add_service(NemrServer::new(service))
        .serve_with_incoming_shutdown(incoming, shutdown)
        .await
        .context("nemrd server error")?;

    // Only if the path still names the socket we bound. If something replaced
    // it while we were running, removing it would take out a live daemon on the
    // way past — the same mistake as the unlink-before-bind above, one exit
    // later.
    let still_ours = std::fs::metadata(&path).ok().map(|m| {
        use std::os::unix::fs::MetadataExt;
        (m.dev(), m.ino())
    });
    if ours.is_some() && still_ours == ours {
        let _ = std::fs::remove_file(&path);
    }
    Ok(())
}
