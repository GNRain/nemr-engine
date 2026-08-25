//! Daemon side of `nemr attach` (E-09): drive a container exec while streaming
//! its IO to and from a gRPC client, instead of the local terminal.
//!
//! The split with the CLI is deliberate. The daemon owns everything containerd:
//! the exec, the FIFOs, `resize_pty`, `close_exec_stdin`, `wait_exec`,
//! `delete_exec`. The CLI owns everything terminal: raw mode, `SIGWINCH`, the
//! private-mode restoration — knowledge the daemon does not have and should not.
//! The CLI ships its terminal knowledge as stream messages (the initial window
//! size, resizes, a pipe reaching EOF); the daemon acts on them. So the proven
//! pty/EOF/resize logic is preserved, only its "terminal" edge is replaced by
//! the stream.

use crate::containerd::client::ContainerdClient;
use crate::proto::attach_client;
use crate::proto::attach_server;
use crate::proto::{AttachClient, AttachServer, AttachStarted};
use std::io::{Read, Write};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Response, Status, Streaming};

pub type AttachStream = ReceiverStream<Result<AttachServer, Status>>;

fn out(msg: attach_server::Msg) -> Result<AttachServer, Status> {
    Ok(AttachServer { msg: Some(msg) })
}

pub async fn serve(
    client: Arc<ContainerdClient>,
    mut incoming: Streaming<AttachClient>,
) -> Result<Response<AttachStream>, Status> {
    // The first message must be Start; it carries the client's terminal facts.
    let first = incoming
        .message()
        .await
        .map_err(|e| Status::internal(format!("attach stream error: {e}")))?
        .ok_or_else(|| Status::invalid_argument("attach: empty stream"))?;
    let start = match first.msg {
        Some(attach_client::Msg::Start(s)) => s,
        _ => {
            return Err(Status::invalid_argument(
                "attach: first message must be Start",
            ))
        }
    };

    let session = crate::engine::project::attach_exec_start(
        &client,
        &start.name,
        start.interactive,
        start.rows as u16,
        start.cols as u16,
    )
    .await
    .map_err(super::status_from_anyhow)?;

    // The response channel. Bounded so a slow client applies backpressure rather
    // than letting the daemon buffer without limit.
    let (tx, rx) = mpsc::channel::<Result<AttachServer, Status>>(64);

    tokio::spawn(run_session(client, session, incoming, tx));

    Ok(Response::new(ReceiverStream::new(rx)))
}

async fn run_session(
    client: Arc<ContainerdClient>,
    session: crate::engine::project::AttachExec,
    mut incoming: Streaming<AttachClient>,
    tx: mpsc::Sender<Result<AttachServer, Status>>,
) {
    let crate::engine::project::AttachExec {
        container_id,
        exec_id,
        io_dir,
        stdin_fifo,
        stdout_fifo,
        stderr_fifo,
        terminal,
    } = session;

    let _ = tx
        .send(out(attach_server::Msg::Started(AttachStarted {})))
        .await;

    // Output pumps: blocking FIFO reads on their own threads, forwarding chunks
    // into the async response channel. A FIFO opened O_RDWR never reports EOF on
    // its own, so a stop flag tells them to drain and finish (the same reason
    // the terminal-bound attach uses `pump_until_stopped`).
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let out_stop = stop.clone();
    let out_tx = tx.clone();
    let mut stdout_fifo = stdout_fifo;
    let stdout_thread = std::thread::spawn(move || {
        pump_fifo_to_stream(&mut stdout_fifo, out_stop, out_tx, false);
    });
    let stderr_thread = stderr_fifo.map(|mut fifo| {
        let stop = stop.clone();
        let tx = tx.clone();
        std::thread::spawn(move || pump_fifo_to_stream(&mut fifo, stop, tx, true))
    });

    // The stdin FIFO write end lives here so we can close it to signal EOF.
    let mut stdin_write: Option<std::fs::File> = Some(stdin_fifo);

    // Fallback timer for a pty whose program ignores EOT (armed on StdinEof).
    let fallback = tokio::time::sleep(std::time::Duration::from_secs(0));
    tokio::pin!(fallback);
    let mut fallback_armed = false;

    let exit_code = loop {
        tokio::select! {
            status = client.wait_exec(&container_id, &exec_id) => {
                break status.unwrap_or(-1i32 as u32) as i32;
            }

            msg = incoming.message() => {
                match msg {
                    Ok(Some(AttachClient { msg: Some(m) })) => {
                        match m {
                            attach_client::Msg::Stdin(bytes) => {
                                if let Some(w) = stdin_write.as_mut() {
                                    let _ = w.write_all(&bytes);
                                    let _ = w.flush();
                                }
                            }
                            attach_client::Msg::Resize(r) => {
                                let _ = client
                                    .resize_pty(&container_id, &exec_id, r.cols, r.rows)
                                    .await;
                            }
                            attach_client::Msg::StdinEof(_) => {
                                // A pipe reached EOF. For a pty, EOT (0x04) is
                                // the polite signal, with a fallback; for a
                                // pipe, dropping the write end IS the EOF, so
                                // the shim's copier finally sees it.
                                if terminal {
                                    if let Some(w) = stdin_write.as_mut() {
                                        let _ = w.write_all(&[0x04]);
                                        let _ = w.flush();
                                    }
                                    fallback.as_mut().reset(
                                        tokio::time::Instant::now()
                                            + std::time::Duration::from_secs(10),
                                    );
                                    fallback_armed = true;
                                } else {
                                    drop(stdin_write.take());
                                    let _ = client
                                        .close_exec_stdin(&container_id, &exec_id)
                                        .await;
                                }
                            }
                            attach_client::Msg::Start(_) => {
                                // A second Start is a client bug; ignore it.
                            }
                        }
                    }
                    Ok(Some(_)) => {}       // empty message frame
                    Ok(None) => {
                        // Client hung up. Close stdin so the shell can exit, then
                        // keep waiting for the exec to finish.
                        drop(stdin_write.take());
                        let _ = client.close_exec_stdin(&container_id, &exec_id).await;
                    }
                    Err(_) => {
                        drop(stdin_write.take());
                        let _ = client.close_exec_stdin(&container_id, &exec_id).await;
                    }
                }
            }

            _ = &mut fallback, if fallback_armed => {
                fallback_armed = false;
                let _ = client.close_exec_stdin(&container_id, &exec_id).await;
            }
        }
    };

    let _ = client.close_exec_stdin(&container_id, &exec_id).await;
    let _ = client.delete_exec(&container_id, &exec_id).await;

    // Drain and stop the output pumps: we hold a write end of the FIFO, so they
    // would never see EOF and joining would hang otherwise.
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = stdout_thread.join();
    if let Some(handle) = stderr_thread {
        let _ = handle.join();
    }

    let _ = tx.send(out(attach_server::Msg::ExitCode(exit_code))).await;

    let _ = std::fs::remove_dir_all(&io_dir);
}

/// Blocking pump: read a FIFO until stopped, forwarding chunks into the async
/// response channel as Stdout/Stderr messages.
fn pump_fifo_to_stream(
    fifo: &mut std::fs::File,
    stop: Arc<std::sync::atomic::AtomicBool>,
    tx: mpsc::Sender<Result<AttachServer, Status>>,
    is_stderr: bool,
) {
    use std::os::unix::io::AsRawFd;
    // Non-blocking + poll, so we can honour the stop flag rather than blocking
    // forever in read() on a FIFO that never reports EOF.
    let fd = fifo.as_raw_fd();
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags != -1 {
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }
    let mut buf = [0u8; 8192];
    loop {
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut pfd, 1, 100) };
        if ready > 0 && (pfd.revents & libc::POLLIN) != 0 {
            match fifo.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let chunk = buf[..n].to_vec();
                    let msg = if is_stderr {
                        attach_server::Msg::Stderr(chunk)
                    } else {
                        attach_server::Msg::Stdout(chunk)
                    };
                    if tx.blocking_send(out(msg)).is_err() {
                        break; // client gone
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => break,
            }
        } else if stop.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
    }
}
