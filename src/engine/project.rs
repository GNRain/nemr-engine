//! Project lifecycle: create (Milestone 4); start/stop/attach and list/delete
//! follow in Milestones 5 and 6.
//!
//! A project is the user-facing unit of isolation (Section 2): one named,
//! quota-bounded volume paired with one container built from the base image.

use std::collections::HashMap;

use anyhow::{bail, Context, Result};

use crate::auth;
use crate::config;
use crate::containerd::client::ContainerdClient;
use crate::containerd::containers::{BindMount, ContainerSpec, StopOutcome};
use crate::engine::volume::{HelperOps, PrivilegedOps, Volume, VolumePaths, VolumeSize};

/// Runs in the container's shell before every prompt, so a prompt never lands
/// on top of the previous command's output.
///
/// # The problem
///
/// A full-screen program interrupted with Ctrl+C — Claude Code being the case
/// that matters here — exits with the cursor wherever it happened to be
/// rendering, and often with the cursor hidden and colours still set. Bash then
/// draws its next prompt at that position, so the prompt and everything typed
/// afterwards overwrites the program's output. Recovering means running
/// `clear`, which is a poor thing to ask of every user after every session.
///
/// # The fix
///
/// Three parts, all emitted before each prompt:
///
/// 1. Show the cursor and reset attributes, undoing what the interrupted
///    program left set.
/// 2. Move to a fresh line, but *only* when the previous output ended
///    mid-line. Printing exactly `$COLUMNS` spaces from column `c` lands the
///    cursor at column `c` of the next line when `c > 0`, and — thanks to
///    deferred wrap — leaves it on the current line when `c == 0`. The
///    following `\r` returns to column 0. So a command that ended cleanly gets
///    no blank line, and one that ended mid-line gets exactly one. This is the
///    partial-line trick zsh uses for `PROMPT_SP`, with spaces instead of
///    zsh's inverse `%` marker so nothing visible is left behind.
/// 3. Erase from the cursor to the end of the screen (`\e[J`).
///
/// Step 3 is what handles the interrupted-TUI case, and step 2 alone does not.
/// An Ink-based program like Claude Code re-renders by moving the cursor *up*
/// over its own frame; interrupted mid-frame, it leaves the cursor above output
/// that is still on screen, at column 0. Bash then draws its prompt there, and
/// the prompt — plus everything typed after it — overwrites the stale frame
/// line by line. Measured against a terminal emulator, the prompt landed on top
/// of "claude output line 7" and `logout` on top of line 8.
///
/// Erasing below the cursor is safe at prompt time because a well-behaved
/// command leaves the cursor after its last line, where there is nothing to
/// erase. Anything still below is a frame nobody is managing any more, and it
/// is going to be overwritten regardless — erased is strictly better than
/// garbled. Scrollback above the cursor is untouched, so the session's history
/// remains readable.
///
/// Set through the exec's environment rather than the image, because it is a
/// property of an interactive attach session rather than of the image itself —
/// and bash reads `PROMPT_COMMAND` from the environment.
const PROMPT_TIDY: &str = concat!(
    "PROMPT_COMMAND=",
    r#"printf '\e[?25h\e[0m%*s\r\e[J' "${COLUMNS:-80}" ''"#,
);

/// Label keys written onto the container record.
///
/// Prefixed so engine metadata is distinguishable from anything else that
/// might label a container in this namespace.
pub const LABEL_PROJECT: &str = "nemr.project";
pub const LABEL_VOLUME: &str = "nemr.volume";
pub const LABEL_SIZE: &str = "nemr.size";

/// Create a project: a quota-bounded volume plus a ready-to-start container.
///
/// # Ordering
///
/// The sequence is chosen so that failures cost as little as possible and
/// never leave partial state:
///
/// 1. Validate the name, and reject a duplicate **before** touching anything.
/// 2. Resolve host credentials (AUTH-03). This fails fast, before a volume is
///    allocated, because the fix is on the user's side and there is no point
///    provisioning storage for a container that could not authenticate.
/// 3. Create and mount the volume (Milestone 3).
/// 4. Create the container. If this fails, the volume guard's `Drop` releases
///    the mount and loop device, and the backing file is removed.
/// 5. Only once the container exists, `persist()` the volume so it outlives
///    the guard — the container now depends on it.
pub async fn create(
    client: &ContainerdClient,
    name: &str,
    size: VolumeSize,
) -> Result<ProjectSummary> {
    crate::engine::volume::validate_name(name)
        .with_context(|| format!("invalid project name {name:?}"))?;

    let container_id = config::container_id(name);
    let paths = VolumePaths::from_env()?;

    // AC-4.2: a duplicate must fail clearly rather than overwrite or produce a
    // half-owned pair. Check both halves — either one existing means the name
    // is taken, and a project with only one half is a broken state we should
    // report rather than silently complete.
    if client.container_exists(&container_id).await? {
        bail!(
            "project {name:?} already exists (container {container_id:?}).\n\
             Choose a different name, or delete the existing project first."
        );
    }
    if paths.image_file(name).exists() {
        bail!(
            "project {name:?} already has a volume at {}, but no container.\n\
             This is a partially-created project; remove the volume file before retrying.",
            paths.image_file(name).display()
        );
    }

    // AUTH-01/02/03: credentials come from the host, read-only, and their
    // absence is a clear failure before anything is provisioned.
    let credentials = auth::resolve_credentials()?;
    auth::check_permissions(&credentials)?;

    let volume = Volume::create(name, size, paths, HelperOps::new())
        .with_context(|| format!("failed to provision volume for project {name:?}"))?;
    let mount_point = volume.mount_point();

    // M8: create the on-volume directories that hold the relocated session state,
    // before the container binds them in. They are created on the mounted volume
    // (chowned to the invoker, so they map to root inside the container) so that
    // Claude Code's conversation history is written to the layer that travels.
    // See `session_state_mounts` and docs/state-locality.md.
    for subdir in [config::VOLUME_STATE_PROJECTS, config::VOLUME_STATE_SESSIONS] {
        let dir = mount_point.join(subdir);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create session-state dir {}", dir.display()))?;
    }

    let mut labels = HashMap::new();
    labels.insert(LABEL_PROJECT.to_string(), name.to_string());
    labels.insert(LABEL_VOLUME.to_string(), mount_point.to_string_lossy().to_string());
    labels.insert(LABEL_SIZE.to_string(), size.to_string());

    let mut mounts = vec![
        // The project volume becomes the container's working directory.
        BindMount::read_write(&mount_point, config::CONTAINER_WORKDIR),
        // AUTH-02: credentials read-only, and only the credentials file — no
        // other host-side ~/.claude content. This stays a host bind mount and is
        // NOT relocated onto the volume (D-02): the credential must never travel.
        BindMount::read_only(&credentials, config::CONTAINER_CREDENTIALS),
    ];
    // M8: bind the session-critical subtrees from the volume over their rootfs
    // locations, so history and session state live on the portable layer.
    mounts.extend(session_state_mounts(&mount_point));

    let spec = ContainerSpec {
        id: container_id.clone(),
        image: config::BASE_IMAGE.to_string(),
        mounts,
        working_dir: Some(config::CONTAINER_WORKDIR.to_string()),
        extra_env: vec![],
        args: Some(config::SUPERVISOR_ARGS.iter().map(|s| s.to_string()).collect()),
        // Bare project name: the scope is `nemr-<name>.scope`, and passing the
        // container id (already `nemr-` prefixed) would double it.
        cgroup_name: Some(name.to_string()),
        cgroup_prefix: config::CGROUP_PREFIX.to_string(),
        labels,
    };

    if let Err(error) = client.create_container(&spec).await {
        // `volume` is still owned here, so returning drops it and releases the
        // mount and loop device. Remove the backing file too, or the duplicate
        // check above would reject a retry.
        drop(volume);
        let _ = std::fs::remove_file(VolumePaths::from_env()?.image_file(name));
        return Err(error).with_context(|| {
            format!("failed to create container for project {name:?}; volume released")
        });
    }

    // Committed: the container depends on this mount now.
    let mount_point = volume.persist();

    Ok(ProjectSummary {
        name: name.to_string(),
        container_id,
        volume_path: mount_point.to_string_lossy().to_string(),
        size,
    })
}

/// Bind mounts that relocate Claude Code's session-critical state onto the
/// portable volume (M8).
///
/// Each maps a directory on the volume over the rootfs location Claude Code
/// writes to, so the conversation history and session state land on the layer
/// that travels with a bundle. Credentials (`CONTAINER_CREDENTIALS`) and the
/// identity-bearing `/root/.claude.json` are deliberately absent — they stay on
/// the rootfs so they cannot travel (D-02). See `docs/state-locality.md`.
fn session_state_mounts(mount_point: &std::path::Path) -> Vec<BindMount> {
    vec![
        BindMount::read_write(
            mount_point.join(config::VOLUME_STATE_PROJECTS),
            config::CONTAINER_CLAUDE_PROJECTS,
        ),
        BindMount::read_write(
            mount_point.join(config::VOLUME_STATE_SESSIONS),
            config::CONTAINER_CLAUDE_SESSIONS,
        ),
    ]
}

/// What `create` produced, for the CLI to report.
#[derive(Debug, Clone)]
pub struct ProjectSummary {
    pub name: String,
    pub container_id: String,
    pub volume_path: String,
    pub size: VolumeSize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_keys_are_namespaced() {
        for key in [LABEL_PROJECT, LABEL_VOLUME, LABEL_SIZE] {
            assert!(key.starts_with("nemr."), "{key} should be namespaced");
        }
    }

    #[test]
    fn container_id_is_prefixed() {
        assert_eq!(config::container_id("demo"), "nemr-demo");
    }
}

/// Resolve a project name to its container, failing clearly if absent.
async fn resolve(client: &ContainerdClient, name: &str) -> Result<String> {
    crate::engine::volume::validate_name(name)
        .with_context(|| format!("invalid project name {name:?}"))?;

    let container_id = config::container_id(name);
    if !client.container_exists(&container_id).await? {
        bail!(
            "no project named {name:?}.\n\
             Create it first: nemr create {name} --size 2GB"
        );
    }
    Ok(container_id)
}

/// Start a project's container (Milestone 5).
///
/// PID 1 is the supervisor from PROC-01; no interactive session is created
/// here. `attach` is what gives a shell.
pub async fn start(client: &ContainerdClient, name: &str) -> Result<u32> {
    let container_id = resolve(client, name).await?;

    // VOL-06. Container records live in containerd's database and survive a
    // reboot; mounts and loop devices do not. Starting without checking gives
    // the container an empty /workspace backed by whatever filesystem the mount
    // point directory happens to sit on — the host root filesystem, with no
    // quota. Nothing errors, so a user can work an entire session believing
    // they are writing to their project. That is a silent VOL-05 violation, and
    // this check is what closes it.
    ensure_volume_mounted(name)?;

    // AC-5.3: starting an already-running project is a clear error, not a
    // second task or a silent no-op.
    match client.task_state(&container_id).await? {
        state if state.is_running() => bail!(
            "project {name:?} is already running.\n\
             Attach to it with: nemr attach {name}"
        ),
        crate::containerd::containers::TaskState::Stopped => {
            // A task that exited but was never reaped would block a new one.
            client.stop_task(&container_id).await?;
        }
        _ => {}
    }

    let pid = client.start_task(&container_id).await?;
    Ok(pid)
}

/// Stop a project's container (Milestone 5).
///
/// Returns how the task actually stopped, so the caller can distinguish a
/// clean shutdown from one that had to be killed (PROC-06). Reporting only
/// success is what let a supervisor that ignored SIGTERM go unnoticed for the
/// whole of Phase 1.
pub async fn stop(client: &ContainerdClient, name: &str) -> Result<StopOutcome> {
    let container_id = resolve(client, name).await?;

    // AC-5.3: stopping an already-stopped project must fail clearly rather
    // than report success for work it did not do.
    if client.task_state(&container_id).await?
        == crate::containerd::containers::TaskState::None
    {
        bail!(
            "project {name:?} is not running.\n\
             Start it with: nemr start {name}"
        );
    }

    client.stop_task(&container_id).await
}

/// Remove FIFO directories left behind by attach processes that are gone.
///
/// A clean exit removes its own directory. One killed outright cannot, so these
/// accumulate in `$XDG_RUNTIME_DIR` (NFR-03). Each directory carries the PID
/// that created it, so a live session's directory is never touched — deleting
/// one belonging to a long-running attach would break it, which rules out
/// simpler age-based sweeping.
///
/// Best-effort throughout: this is tidying, and must never obstruct an attach.
fn sweep_stale_attach_dirs() {
    let Some(runtime_dir) = std::env::var_os("XDG_RUNTIME_DIR") else {
        return;
    };
    let base = std::path::PathBuf::from(runtime_dir).join("nemr");
    let Ok(entries) = std::fs::read_dir(&base) else {
        return;
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };

        // attach-<pid>-<nanos>
        let Some(pid) = name
            .strip_prefix("attach-")
            .and_then(|rest| rest.split('-').next())
            .and_then(|pid| pid.parse::<u32>().ok())
        else {
            continue;
        };

        // /proc/<pid> existing is the liveness test; absent means the creator
        // is gone and the directory is safe to remove.
        if !std::path::Path::new(&format!("/proc/{pid}")).exists() {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Ensure a project's volume is mounted, remounting it if not (VOL-06).
///
/// Remount rather than refuse: the backing file is intact and the privileged
/// helper already knows how to attach and mount it, so requiring the user to
/// repair this by hand would defeat the portability the product exists for.
/// Failure to remount *is* fatal — proceeding is what this guards against.
#[tracing::instrument(name = "ensure_volume_mounted", skip_all, fields(project = %name))]
pub fn ensure_volume_mounted(name: &str) -> Result<()> {
    let paths = VolumePaths::from_env()?;
    let mount_point = paths.mount_point(name);

    // The VOL-05 decision point. Log what was checked and what the answer was —
    // not just the action taken. VOL-05 was invisible in the log precisely
    // because "proceeding" and "proceeding against the wrong filesystem" printed
    // the same nothing.
    let mounted = crate::engine::volume::is_mounted(&mount_point);
    tracing::debug!(
        mount_point = %mount_point.display(),
        mounted,
        backing_device = %crate::engine::volume::backing_device(&mount_point)
            .unwrap_or_else(|| "<none>".into()),
        "checked whether the project volume is mounted"
    );

    if mounted {
        return Ok(());
    }

    let image = paths.image_file(name);
    if !image.exists() {
        bail!(
            "project {name:?} has no backing file at {}.\n\
             The volume is gone; the project cannot be started. Delete it with \
             `nemr delete {name}` and create it again.",
            image.display()
        );
    }

    // The size preset is needed only for logging inside the helper; the volume
    // already exists and is not resized here.
    let size = read_recorded_size(&paths, name).unwrap_or(VolumeSize::DEFAULT);

    audit_remount(name, &mount_point);
    std::fs::create_dir_all(&mount_point)
        .with_context(|| format!("failed to recreate mount point {}", mount_point.display()))?;

    HelperOps::new()
        .attach_and_mount(name, size)
        .with_context(|| {
            format!(
                "failed to remount the volume for project {name:?}.\n\
                 Refusing to start: the container would otherwise run against \
                 {} on the host filesystem, with no quota and none of the \
                 project's data.",
                mount_point.display()
            )
        })?;

    // Confirm the remount actually landed, and say which device backs it. This
    // is the line that turns VOL-05 from "found after a reboot by hand" into
    // "obvious on the first run": a working directory backed by the host root
    // device instead of a loop device is visible right here.
    let device = crate::engine::volume::backing_device(&mount_point);
    tracing::debug!(
        mount_point = %mount_point.display(),
        remounted = crate::engine::volume::is_mounted(&mount_point),
        backing_device = %device.clone().unwrap_or_else(|| "<none>".into()),
        "remounted the project volume"
    );
    tracing::info!(
        "remounted volume for {name:?} from {} ({})",
        image.display(),
        device.unwrap_or_else(|| "unknown device".into())
    );

    Ok(())
}

/// Best-effort recovery of the size a volume was created with.
///
/// Derived from the backing file's apparent size, which is exactly the preset
/// requested at creation — the file is sparse, so this costs nothing to read
/// and does not depend on containerd being reachable.
fn read_recorded_size(paths: &VolumePaths, name: &str) -> Option<VolumeSize> {
    let length = std::fs::metadata(paths.image_file(name)).ok()?.len();
    VolumeSize::all().into_iter().find(|s| s.bytes() == length)
}

fn audit_remount(name: &str, mount_point: &std::path::Path) {
    tracing::info!(
        "[nemr:volume] volume for {name:?} is not mounted at {}; remounting (VOL-06)",
        mount_point.display()
    );
}

/// Whether a project is currently running.
pub async fn is_running(client: &ContainerdClient, name: &str) -> Result<bool> {
    let container_id = config::container_id(name);
    Ok(client.task_state(&container_id).await?.is_running())
}

/// Run one command in a running project and capture its stdout and exit code.
///
/// A non-interactive counterpart to [`attach`]: no TTY, no stdin, output
/// collected rather than streamed. Used by health checks and by the regression
/// suite to read the container's own view of a path (e.g. to prove that
/// relocated session state is visible where Claude Code writes it), without
/// depending on the streaming attach machinery.
pub async fn exec_capture(
    client: &ContainerdClient,
    name: &str,
    argv: &[&str],
) -> Result<(u32, String)> {
    use crate::containerd::containers::ExecIo;
    use crate::engine::tty;
    use std::io::Write;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    let container_id = resolve(client, name).await?;
    if !client.task_state(&container_id).await?.is_running() {
        bail!("project {name:?} is not running; start it first");
    }

    let exec_id = format!(
        "capture-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let io_dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .context("XDG_RUNTIME_DIR is not set; cannot place exec FIFOs")?
        .join("nemr")
        .join(&exec_id);
    std::fs::create_dir_all(&io_dir)
        .with_context(|| format!("failed to create {}", io_dir.display()))?;

    let io = ExecIo {
        stdin: io_dir.join("stdin"),
        stdout: io_dir.join("stdout"),
        stderr: Some(io_dir.join("stderr")),
        terminal: false,
    };
    tty::make_fifo(&io.stdin)?;
    tty::make_fifo(&io.stdout)?;
    if let Some(stderr) = &io.stderr {
        tty::make_fifo(stderr)?;
    }

    let process = serde_json::json!({
        "terminal": false,
        "user": { "uid": 0, "gid": 0 },
        "args": argv,
        "env": [
            "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
            "HOME=/root",
        ],
        "cwd": "/",
        "noNewPrivileges": true
    });

    client.exec_process(&container_id, &exec_id, process, &io).await?;

    let stdin_fifo = tty::open_fifo(&io.stdin)?;
    let stdout_fifo = tty::open_fifo(&io.stdout)?;
    let stderr_fifo = io.stderr.as_ref().map(|p| tty::open_fifo(p)).transpose()?;

    client.start_exec(&container_id, &exec_id).await?;

    // No stdin: close our write end so the process sees EOF immediately.
    drop(stdin_fifo);
    let _ = client.close_exec_stdin(&container_id, &exec_id).await;

    // Collect stdout on a thread until the exec exits (the FIFO is O_RDWR, so it
    // never reports EOF on its own — same reason attach uses a stop flag).
    let collected = Arc::new(Mutex::new(Vec::<u8>::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let out_writer = SharedWriter(collected.clone());
    let out_stop = stop.clone();
    let out_thread =
        std::thread::spawn(move || tty::pump_until_stopped(stdout_fifo, out_writer, out_stop, None));
    let err_thread = stderr_fifo.map(|fifo| {
        let stop = stop.clone();
        std::thread::spawn(move || tty::pump_until_stopped(fifo, std::io::sink(), stop, None))
    });

    let exit = client.wait_exec(&container_id, &exec_id).await?;
    let _ = client.delete_exec(&container_id, &exec_id).await;

    stop.store(true, Ordering::Relaxed);
    let _ = out_thread.join();
    if let Some(handle) = err_thread {
        let _ = handle.join();
    }
    let _ = std::fs::remove_dir_all(&io_dir);

    let stdout = collected
        .lock()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default();

    // A tiny local Write adapter, so pump_until_stopped can collect into a Vec.
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);
    impl Write for SharedWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if let Ok(mut guard) = self.0.lock() {
                guard.extend_from_slice(buf);
            }
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    Ok((exit, stdout))
}

/// Attach an interactive session to a running project (Milestone 5).
///
/// Per PROC-02 this is a **task exec with its own TTY**, not a connection to
/// PID 1's terminal. Exiting the shell ends only this exec; the project keeps
/// running, and concurrent attaches get independent terminals.
pub async fn attach(client: &ContainerdClient, name: &str) -> Result<u32> {
    use crate::containerd::containers::ExecIo;
    use crate::engine::tty;

    let container_id = resolve(client, name).await?;

    // AC-5.3: attaching to a project that was never started must fail clearly,
    // not hang waiting for a task that does not exist.
    if !client.task_state(&container_id).await?.is_running() {
        bail!(
            "project {name:?} is not running.\n\
             Start it first: nemr start {name}"
        );
    }

    // A unique exec id per attach, so concurrent sessions do not collide. The
    // PID is included so a directory left behind by a killed process can be
    // identified and swept later — see `sweep_stale_attach_dirs`.
    let exec_id = format!(
        "attach-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );

    sweep_stale_attach_dirs();

    let io_dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .context("XDG_RUNTIME_DIR is not set; cannot place attach FIFOs")?
        .join("nemr")
        .join(&exec_id);
    std::fs::create_dir_all(&io_dir)
        .with_context(|| format!("failed to create {}", io_dir.display()))?;

    // A pty only when stdin really is a terminal. This is what `docker exec -t`
    // does, and it matters for more than cosmetics: a pty has no EOF, so a
    // scripted `echo cmd | nemr attach` could never tell the shell its input had
    // finished. Neither writing EOT nor containerd's CloseIO ends a pty-backed
    // session — both were tried, and both hung indefinitely. Without a terminal
    // stdin is an ordinary pipe, closing it is a real EOF, and the shell exits
    // on its own with its own status.
    let use_terminal = tty::stdin_is_terminal();

    let io = ExecIo {
        stdin: io_dir.join("stdin"),
        stdout: io_dir.join("stdout"),
        // A pty merges stderr into the same stream; only a pipe-backed session
        // needs a separate one.
        stderr: (!use_terminal).then(|| io_dir.join("stderr")),
        terminal: use_terminal,
    };
    tty::make_fifo(&io.stdin)?;
    tty::make_fifo(&io.stdout)?;
    if let Some(stderr) = &io.stderr {
        tty::make_fifo(stderr)?;
    }

    let process = serde_json::json!({
        "terminal": use_terminal,
        "user": { "uid": 0, "gid": 0 },
        "args": ["/bin/bash", "-l"],
        "env": [
            "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
            "HOME=/root",
            "TERM=".to_string() + &std::env::var("TERM").unwrap_or_else(|_| "xterm".into()),
            "USE_BUILTIN_RIPGREP=0".to_string(),
            PROMPT_TIDY.to_string(),
        ],
        "cwd": config::CONTAINER_WORKDIR,
        "capabilities": {
            "bounding":  ["CAP_CHOWN","CAP_DAC_OVERRIDE","CAP_FSETID","CAP_FOWNER","CAP_MKNOD",
                          "CAP_NET_RAW","CAP_SETGID","CAP_SETUID","CAP_SETFCAP","CAP_SETPCAP",
                          "CAP_NET_BIND_SERVICE","CAP_SYS_CHROOT","CAP_KILL","CAP_AUDIT_WRITE"],
            "effective": ["CAP_CHOWN","CAP_DAC_OVERRIDE","CAP_FSETID","CAP_FOWNER","CAP_MKNOD",
                          "CAP_NET_RAW","CAP_SETGID","CAP_SETUID","CAP_SETFCAP","CAP_SETPCAP",
                          "CAP_NET_BIND_SERVICE","CAP_SYS_CHROOT","CAP_KILL","CAP_AUDIT_WRITE"],
            "permitted": ["CAP_CHOWN","CAP_DAC_OVERRIDE","CAP_FSETID","CAP_FOWNER","CAP_MKNOD",
                          "CAP_NET_RAW","CAP_SETGID","CAP_SETUID","CAP_SETFCAP","CAP_SETPCAP",
                          "CAP_NET_BIND_SERVICE","CAP_SYS_CHROOT","CAP_KILL","CAP_AUDIT_WRITE"]
        },
        "noNewPrivileges": true
    });

    client
        .exec_process(&container_id, &exec_id, process, &io)
        .await?;

    // Open both FIFOs before starting, so no output is lost in the gap between
    // the process starting and us being ready to read.
    let stdin_fifo = tty::open_fifo(&io.stdin)?;
    let stdout_fifo = tty::open_fifo(&io.stdout)?;
    let stderr_fifo = io.stderr.as_ref().map(|p| tty::open_fifo(p)).transpose()?;

    // Raw mode is enabled only once the exec is about to run, and the guard
    // restores the terminal on every exit path below.
    let _raw = tty::RawMode::enable()?;

    client.start_exec(&container_id, &exec_id).await?;

    if let Some((width, height)) = tty::window_size() {
        let _ = client.resize_pty(&container_id, &exec_id, width, height).await;
    }

    // Blocking IO off the async runtime. The gRPC side stays async; mixing is
    // simpler here than making FIFO reads async.
    //
    // The stdin pump is a tracked task rather than a detached thread because
    // its *completion* is load-bearing: when local input is exhausted the exec
    // must be told, or a shell reading piped input never sees EOF and never
    // exits. See the select loop below.
    // A second handle on the stdin FIFO, kept so EOT can be written after the
    // pump has consumed local input and given up ownership of its copy.
    // Kept so end-of-input can be signalled after the pump has finished with
    // its own copy. Held in an Option because, for a pipe-backed session,
    // *dropping* it is the signal: the shim only sees EOF on the FIFO once
    // every write end is closed, and ours would otherwise hold it open forever.
    let mut eof_handle = Some(
        stdin_fifo
            .try_clone()
            .context("failed to duplicate the stdin FIFO handle")?,
    );

    // A plain thread, deliberately not `spawn_blocking`.
    //
    // Interactively this pump blocks in `read()` on the user's terminal, which
    // never reaches EOF, so it can never finish. Tokio cannot cancel a blocking
    // task once it has started, and dropping the runtime *waits* for the
    // blocking pool to drain — so the process could not exit after the shell
    // did. The symptom was `exit` printing "logout" and then hanging forever,
    // leaving the terminal unusable.
    //
    // A detached OS thread dies with the process instead. Completion is
    // reported over a channel, since `select!` still needs to know when local
    // input has run out.
    let (stdin_done_tx, stdin_done) = tokio::sync::oneshot::channel::<()>();
    std::thread::spawn(move || {
        tty::pump(std::io::stdin(), stdin_fifo);
        if std::env::var_os("NEMR_DEBUG_ATTACH").is_some() {
            eprintln!("[debug] stdin pump finished");
        }
        // Failure means the receiver is gone because the session already
        // ended, which is not an error.
        let _ = stdin_done_tx.send(());
    });
    tokio::pin!(stdin_done);
    // The output pump needs an explicit stop signal rather than relying on EOF;
    // see `pump_until_stopped` for why a FIFO opened O_RDWR never reports one.
    let output_stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let pump_stop = output_stop.clone();

    // Only an interactive session can leave the local terminal in a bad state,
    // so only that one is worth watching.
    let modes = use_terminal
        .then(|| std::sync::Arc::new(std::sync::Mutex::new(tty::ModeTracker::default())));
    let pump_modes = modes.clone();

    let from_container = std::thread::spawn(move || {
        tty::pump_until_stopped(stdout_fifo, std::io::stdout(), pump_stop, pump_modes);
    });

    let errors_from_container = stderr_fifo.map(|fifo| {
        let stop = output_stop.clone();
        std::thread::spawn(move || {
            tty::pump_until_stopped(fifo, std::io::stderr(), stop, None);
        })
    });

    // Forward window resizes for as long as the session lasts.
    let resize_client = container_id.clone();
    let resize_exec = exec_id.clone();
    let mut winch = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())
        .context("failed to install SIGWINCH handler")?;

    // Fallback timer, armed only if signalling EOF the polite way does not get
    // the process to exit. Created up front so `select!` always has something
    // to poll; it does nothing until armed.
    let fallback = tokio::time::sleep(std::time::Duration::from_secs(0));
    tokio::pin!(fallback);
    let mut fallback_armed = false;
    let mut eof_signalled = false;

    let exit_code = loop {
        tokio::select! {
            status = client.wait_exec(&container_id, &exec_id) => break status?,

            _ = winch.recv() => {
                if let Some((width, height)) = tty::window_size() {
                    let _ = client.resize_pty(&resize_client, &resize_exec, width, height).await;
                }
            }

            // Local input ran out — a pipe or heredoc rather than a terminal.
            // Signal it now, while still waiting: doing it *after* `wait_exec`
            // returns deadlocks, since the shell will not exit until it sees
            // EOF and we would not send EOF until it exits. Interactively this
            // never shows, because a terminal's stdin never reaches EOF.
            _ = &mut stdin_done, if !eof_signalled => {
                eof_signalled = true;
                if std::env::var_os("NEMR_DEBUG_ATTACH").is_some() {
                    eprintln!("[debug] local stdin exhausted; sending EOT");
                }

                if use_terminal {
                    // On a pty, EOF is EOT (0x04) written into the terminal
                    // rather than a closed descriptor. Best-effort: whether it
                    // ends the session depends on the program, hence the
                    // fallback below.
                    if let Some(fifo) = eof_handle.as_ref() {
                        use std::io::Write;
                        let mut fifo = fifo;
                        let _ = fifo.write_all(&[0x04]);
                        let _ = fifo.flush();
                    }

                    fallback.as_mut().reset(
                        tokio::time::Instant::now() + std::time::Duration::from_secs(10),
                    );
                    fallback_armed = true;
                } else {
                    // A pipe. Dropping our handle is what produces the EOF: the
                    // shim's copier is reading this FIFO, and a FIFO only
                    // reports EOF once *every* write end is closed — including
                    // the one this process holds. CloseIO alone did not end the
                    // session, because our handle kept the pipe alive.
                    drop(eof_handle.take());
                    let _ = client.close_exec_stdin(&container_id, &exec_id).await;
                }
            }

            // The process ignored EOT — not a shell, or one not reading stdin.
            // Force the issue rather than waiting forever; the exit status is
            // then SIGHUP-flavoured, but a wrong code beats a hang.
            _ = &mut fallback, if fallback_armed => {
                fallback_armed = false;
                if std::env::var_os("NEMR_DEBUG_ATTACH").is_some() {
                    eprintln!("[debug] EOT ignored; forcing stdin closed");
                }
                let _ = client.close_exec_stdin(&container_id, &exec_id).await;
            }
        }
    };

    if !eof_signalled {
        let _ = client.close_exec_stdin(&container_id, &exec_id).await;
    }
    let _ = client.delete_exec(&container_id, &exec_id).await;

    // Tell the output pump to drain and stop. It cannot detect this itself: we
    // hold a write end of the FIFO, so it would never see EOF and joining it
    // would hang — which is exactly what `attach` used to do on exit.
    output_stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = from_container.join();
    if let Some(handle) = errors_from_container {
        let _ = handle.join();
    }

    // Undo display modes the container's programs left on — and only those.
    // Emitted before the termios guard drops, so the terminal is put back in
    // one pass.
    if let Some(modes) = &modes {
        if let Ok(modes) = modes.lock() {
            let restore = modes.restore_sequence();
            if !restore.is_empty() {
                use std::io::Write;
                let _ = std::io::stdout().write_all(restore.as_bytes());
                let _ = std::io::stdout().flush();
            }
        }
    }

    // The stdin pump thread may still be blocked reading the user's terminal.
    // It is deliberately left alone: it holds nothing the process needs
    // released, and it is torn down when the process exits.
    let _ = std::fs::remove_dir_all(&io_dir);

    Ok(exit_code)
}

/// What a reconciliation sweep found and did.
#[derive(Debug, Default)]
pub struct ReconcileReport {
    /// Orphan mounts/loop devices released (mounted, or a backing file present,
    /// with no owning container record).
    pub released: Vec<String>,
    /// Orphan snapshots removed (a snapshot key with no matching container).
    pub snapshots_removed: Vec<String>,
    /// Backing files with no owning container record. **Reported, not deleted** —
    /// they may hold user data. Their mount and loop device are released, but the
    /// file is left for the operator to remove deliberately.
    pub orphan_backing_files: Vec<String>,
}

impl ReconcileReport {
    pub fn is_empty(&self) -> bool {
        self.released.is_empty()
            && self.snapshots_removed.is_empty()
            && self.orphan_backing_files.is_empty()
    }
}

/// Reclaim host resources whose owning container record is gone (#3/#10/#15/#23).
///
/// # Precedence rule (normative — recorded in SPEC.md Section 11, pending
/// promotion to a Section 3 subsection by the Product Owner)
///
/// **containerd's container records are the single source of truth for which
/// projects exist.** There is no side database. Any host resource — a mount, a
/// loop device, a snapshot — that is not owned by a current container record is
/// an orphan and is reclaimed. The one exception is a backing *file*, which may
/// hold user data: its mount and loop device are released, but the file itself
/// is only reported, never deleted, because destroying data is not something a
/// reconciliation sweep should do unprompted.
///
/// This is the backstop for the one window `delete`'s idempotent ordering cannot
/// cover: a crash after the container record is removed but before the volume is
/// released. Without it, that leaves a mounted, loop-attached volume nothing
/// references and nothing can find. Run it explicitly with `nemr reconcile`, or
/// periodically per `.claude/loop.md`.
pub async fn reconcile_orphans(client: &ContainerdClient) -> Result<ReconcileReport> {
    use std::collections::HashSet;

    let paths = VolumePaths::from_env()?;
    let containers = client.list_containers().await?;

    // The source of truth: names and ids of projects that actually exist.
    let known_names: HashSet<String> = containers
        .iter()
        .filter_map(|c| c.labels.get(LABEL_PROJECT).cloned())
        .collect();
    let known_ids: HashSet<&str> = containers.iter().map(|c| c.id.as_str()).collect();

    let mut report = ReconcileReport::default();
    let helper = HelperOps::new();

    // 1. Orphan mounts: a mount point under the managed dir whose name is not a
    //    known project. Release it (unmount + detach).
    if let Ok(entries) = std::fs::read_dir(paths.mount_dir()) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if known_names.contains(&name) || crate::engine::volume::validate_name(&name).is_err() {
                continue;
            }
            if crate::engine::volume::is_mounted(&paths.mount_point(&name)) {
                match helper.unmount_and_detach(&name) {
                    Ok(()) => report.released.push(name),
                    Err(error) => {
                        eprintln!("[nemr:reconcile] could not release orphan mount {name:?}: {error:#}")
                    }
                }
            }
        }
    }

    // 2. Orphan backing files: an image with no container record. Release any
    //    stray mount/loop, but keep the file (it may hold data) and report it.
    if let Ok(entries) = std::fs::read_dir(paths.image_dir()) {
        for entry in entries.flatten() {
            let file_name = entry.file_name().to_string_lossy().into_owned();
            let Some(name) = file_name.strip_suffix(".img") else {
                continue;
            };
            if known_names.contains(name) || crate::engine::volume::validate_name(name).is_err() {
                continue;
            }
            let _ = helper.unmount_and_detach(name);
            report.orphan_backing_files.push(name.to_string());
        }
    }

    // 3. Orphan snapshots: an engine-created snapshot key with no container.
    for key in client.list_snapshot_keys().await? {
        if key.starts_with(config::CONTAINER_PREFIX) && !known_ids.contains(key.as_str()) {
            match client.remove_snapshot(&key).await {
                Ok(()) => report.snapshots_removed.push(key),
                Err(error) => {
                    eprintln!("[nemr:reconcile] could not remove orphan snapshot {key:?}: {error:#}")
                }
            }
        }
    }

    Ok(report)
}

/// A project as reported by `list`.
#[derive(Debug, Clone)]
pub struct ProjectStatus {
    pub name: String,
    pub container_id: String,
    /// Quota recorded at creation time (the preset that was asked for).
    pub quota: String,
    pub running: bool,
    pub volume_path: String,
    /// Measured usage, absent when the volume is not currently mounted.
    pub usage: Option<crate::engine::volume::Usage>,
}

/// List all projects (Milestone 6).
///
/// State comes from containerd: the container records and their labels are the
/// source of truth, and usage is measured from the mounted filesystem. There is
/// no engine-side database to fall out of step with reality — which is what
/// AC-6.1 is really testing when it cross-checks against `ctr`.
pub async fn list(client: &ContainerdClient) -> Result<Vec<ProjectStatus>> {
    let containers = client.list_containers().await?;
    let mut projects = Vec::new();

    for container in containers {
        // Only containers this engine created are projects. Others in the
        // namespace are none of our business, and reporting them would make
        // `list` disagree with reality in the other direction.
        let Some(name) = container.labels.get(LABEL_PROJECT) else {
            continue;
        };

        let running = client.task_state(&container.id).await?.is_running();
        let volume_path = container
            .labels
            .get(LABEL_VOLUME)
            .cloned()
            .unwrap_or_default();
        let usage = if volume_path.is_empty() {
            None
        } else {
            crate::engine::volume::usage(std::path::Path::new(&volume_path))
        };

        projects.push(ProjectStatus {
            name: name.clone(),
            container_id: container.id.clone(),
            quota: container
                .labels
                .get(LABEL_SIZE)
                .cloned()
                .unwrap_or_else(|| "unknown".into()),
            running,
            volume_path,
            usage,
        });
    }

    projects.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(projects)
}

/// Delete a project and everything it owns (Milestone 6).
///
/// # Ordering: the container record is the anchor, removed last (#3/#10/#20)
///
/// The container record is what `list` and `resolve` use to find a project —
/// there is no side database. So it is removed **last**, only once everything it
/// owns is already gone. The earlier order removed it second, before releasing
/// the volume, which meant a helper failure at the release step stranded a
/// mounted, loop-attached volume that `list` could no longer see, `delete` could
/// no longer resolve to retry, and `create` of the same name rejected with a
/// confusing "volume exists but no container" message. Releasing first is safe
/// because the task is already stopped, so nothing is using the mount; the
/// record referencing the volume path is only metadata.
///
/// Every step is idempotent, so a `delete` interrupted partway is completed by
/// simply running it again: `stop_task` tolerates a missing task, the helper's
/// unmount tolerates an already-released or already-gone volume, and the file
/// removals tolerate absence. The startup reconciliation sweep
/// ([`reconcile_orphans`]) is the backstop for the one window this cannot cover
/// itself — a crash after the record is gone but before the volume is released.
///
/// Confirmation is the caller's responsibility (AC-6.2) — this function does
/// the deleting, the CLI does the asking, so a scripted caller is not fighting
/// a prompt.
pub async fn delete(client: &ContainerdClient, name: &str) -> Result<()> {
    let container_id = resolve(client, name).await?;
    let paths = VolumePaths::from_env()?;

    // 1. Stop the task. Idempotent: stop_task returns NoTask if none is running.
    client.stop_task(&container_id).await?;

    // 2. Release the volume (unmount + detach) BEFORE the record, so a failure
    //    here leaves the project still listable and this delete retryable.
    HelperOps::new()
        .unmount_and_detach(name)
        .with_context(|| format!("failed to release the volume for project {name:?}"))?;

    // 3. Backing file and mount point, now that the loop device is detached.
    let image = paths.image_file(name);
    if image.exists() {
        std::fs::remove_file(&image)
            .with_context(|| format!("failed to remove {}", image.display()))?;
    }
    let mount_point = paths.mount_point(name);
    let _ = std::fs::remove_dir(&mount_point);

    // 4. The container record and its snapshot, LAST. Until this returns, the
    //    project is still discoverable and every prior step is safe to repeat.
    client.delete_container(&container_id).await?;

    Ok(())
}

/// Export a project to a bundle (M9).
///
/// The project must be stopped, so the volume is not being written while it is
/// read. Exporting a running project would capture a transcript mid-append —
/// a torn read that produces a bundle which looks fine and restores a corrupt
/// session, which is the failure shape this project keeps hitting.
pub async fn export(
    client: &ContainerdClient,
    name: &str,
    destination: &std::path::Path,
    policy: crate::bundle::policy::Policy,
) -> crate::error::Result<crate::bundle::export::ExportSummary> {
    use crate::bundle::export::{export as write_bundle, ExportRequest};
    use crate::bundle::manifest::BaseImageRef;
    use crate::error::Error;

    let container_id = config::container_id(name);
    if !client
        .container_exists(&container_id)
        .await
        .map_err(Error::Internal)?
    {
        return Err(Error::NoSuchProject {
            name: name.to_string(),
        });
    }

    if client
        .task_state(&container_id)
        .await
        .map_err(Error::Internal)?
        .is_running()
    {
        return Err(Error::WrongState {
            name: name.to_string(),
            state: "running; stop it first so the volume is not written while it is read",
        });
    }

    // VOL-06: the volume must actually be mounted, or we would export whatever
    // the mount point happens to sit on — the host filesystem.
    ensure_volume_mounted(name).map_err(Error::Internal)?;

    let paths = VolumePaths::from_env().map_err(Error::Internal)?;
    let mount_point = paths.mount_point(name);

    let quota = read_recorded_size(&paths, name)
        .map(|size| size.to_string())
        .unwrap_or_else(|| "unknown".to_string());

    // The base image is referenced by digest, never carried (D-06).
    let digest = client
        .image_target_digest(config::BASE_IMAGE)
        .await
        .map_err(Error::Internal)?;

    let request = ExportRequest {
        project: name,
        quota: &quota,
        source_root: &mount_point,
        base_image: BaseImageRef {
            reference: config::BASE_IMAGE.to_string(),
            digest,
        },
        policy,
    };
    write_bundle(&request, destination)
}

/// Import a bundle into a new project (M10).
///
/// The destination project must already exist and be stopped: creating it is a
/// separate step so the user chooses the quota, and a quota too small for the
/// bundle is refused up front rather than discovered mid-extraction.
///
/// Per D-02 the bundle carries no credential. The caller authenticates on the
/// destination host before attaching; `import` states this rather than leaving
/// it to be discovered at the first API call.
pub async fn import(
    client: &ContainerdClient,
    name: &str,
    bundle_path: &std::path::Path,
) -> crate::error::Result<crate::bundle::import::ExtractSummary> {
    use crate::bundle::import::{open as open_bundle, ImportChecks};
    use crate::error::Error;

    let container_id = config::container_id(name);
    if !client
        .container_exists(&container_id)
        .await
        .map_err(Error::Internal)?
    {
        return Err(Error::NoSuchProject {
            name: name.to_string(),
        });
    }
    if client
        .task_state(&container_id)
        .await
        .map_err(Error::Internal)?
        .is_running()
    {
        return Err(Error::WrongState {
            name: name.to_string(),
            state: "running; stop it before importing over its volume",
        });
    }

    // Read and validate the bundle before touching the destination.
    let bundle = open_bundle(bundle_path)?;

    ensure_volume_mounted(name).map_err(Error::Internal)?;
    let paths = VolumePaths::from_env().map_err(Error::Internal)?;
    let mount_point = paths.mount_point(name);

    // The destination's real usable capacity, measured rather than assumed from
    // the preset — ext4 metadata means the usable total is below the request.
    let usage = crate::engine::volume::usage(&mount_point).ok_or_else(|| {
        Error::host(
            "the destination volume",
            format!("{} is not mounted", mount_point.display()),
            "Start the project once so its volume is mounted, then retry.",
        )
    })?;
    let quota = read_recorded_size(&paths, name)
        .map(|size| size.to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let local_digest = client.image_target_digest(config::BASE_IMAGE).await.ok();
    bundle.check(&ImportChecks {
        local_base_image_digest: local_digest.as_deref(),
        destination_capacity: usage.available,
        destination_quota: &quota,
    })?;

    bundle.extract(&mount_point)
}
