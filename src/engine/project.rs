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
use crate::containerd::containers::{BindMount, ContainerSpec};
use crate::engine::volume::{HelperOps, PrivilegedOps, Volume, VolumePaths, VolumeSize};

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

    let mut labels = HashMap::new();
    labels.insert(LABEL_PROJECT.to_string(), name.to_string());
    labels.insert(LABEL_VOLUME.to_string(), mount_point.to_string_lossy().to_string());
    labels.insert(LABEL_SIZE.to_string(), size.to_string());

    let spec = ContainerSpec {
        id: container_id.clone(),
        image: config::BASE_IMAGE.to_string(),
        mounts: vec![
            // The project volume becomes the container's working directory.
            BindMount::read_write(&mount_point, config::CONTAINER_WORKDIR),
            // AUTH-02: credentials read-only, and only the credentials file —
            // no other host-side ~/.claude content.
            BindMount::read_only(&credentials, config::CONTAINER_CREDENTIALS),
        ],
        working_dir: Some(config::CONTAINER_WORKDIR.to_string()),
        extra_env: vec![],
        args: Some(config::SUPERVISOR_ARGS.iter().map(|s| s.to_string()).collect()),
        // Bare project name: the scope is `nemr-<name>.scope`, and passing the
        // container id (already `nemr-` prefixed) would double it.
        cgroup_name: Some(name.to_string()),
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
pub async fn stop(client: &ContainerdClient, name: &str) -> Result<()> {
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

/// Whether a project is currently running.
pub async fn is_running(client: &ContainerdClient, name: &str) -> Result<bool> {
    let container_id = config::container_id(name);
    Ok(client.task_state(&container_id).await?.is_running())
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

    // A unique exec id per attach, so concurrent sessions do not collide.
    let exec_id = format!(
        "attach-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );

    let io_dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .context("XDG_RUNTIME_DIR is not set; cannot place attach FIFOs")?
        .join("nemr")
        .join(&exec_id);
    std::fs::create_dir_all(&io_dir)
        .with_context(|| format!("failed to create {}", io_dir.display()))?;

    let io = ExecIo {
        stdin: io_dir.join("stdin"),
        stdout: io_dir.join("stdout"),
        terminal: true,
    };
    tty::make_fifo(&io.stdin)?;
    tty::make_fifo(&io.stdout)?;

    let process = serde_json::json!({
        "terminal": true,
        "user": { "uid": 0, "gid": 0 },
        "args": ["/bin/bash", "-l"],
        "env": [
            "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
            "HOME=/root",
            "TERM=".to_string() + &std::env::var("TERM").unwrap_or_else(|_| "xterm".into()),
            format!("USE_BUILTIN_RIPGREP=0"),
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

    // Raw mode is enabled only once the exec is about to run, and the guard
    // restores the terminal on every exit path below.
    let _raw = tty::RawMode::enable()?;

    client.start_exec(&container_id, &exec_id).await?;

    if let Some((width, height)) = tty::window_size() {
        let _ = client.resize_pty(&container_id, &exec_id, width, height).await;
    }

    // Blocking IO on dedicated threads. The gRPC side stays async on the
    // runtime; mixing is simpler here than making FIFO reads async, and these
    // threads exit when their pipe closes.
    let to_container = std::thread::spawn(move || {
        tty::pump(std::io::stdin(), stdin_fifo);
    });
    let from_container = std::thread::spawn(move || {
        tty::pump(stdout_fifo, std::io::stdout());
    });

    // Forward window resizes for as long as the session lasts.
    let resize_client = container_id.clone();
    let resize_exec = exec_id.clone();
    let mut winch = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())
        .context("failed to install SIGWINCH handler")?;

    let exit_code = loop {
        tokio::select! {
            status = client.wait_exec(&container_id, &exec_id) => break status?,
            _ = winch.recv() => {
                if let Some((width, height)) = tty::window_size() {
                    let _ = client.resize_pty(&resize_client, &resize_exec, width, height).await;
                }
            }
        }
    };

    let _ = client.close_exec_stdin(&container_id, &exec_id).await;
    let _ = client.delete_exec(&container_id, &exec_id).await;

    // The output pump ends when the shim closes its end of the FIFO. The stdin
    // pump is blocked reading the user's terminal and will not notice the
    // session ended, so it is left to die with the process rather than joined.
    drop(from_container.join());
    drop(to_container);
    let _ = std::fs::remove_dir_all(&io_dir);

    Ok(exit_code)
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
/// Order matters, and it is the reverse of creation: stop the task, remove the
/// container record and its snapshot, then release the volume, then remove the
/// backing file. Releasing the volume before the container is gone would pull
/// the mount out from under a container that still references it; removing the
/// backing file before the loop device is detached would strand that device
/// permanently, which is the failure Milestone 3 already ran into.
///
/// Confirmation is the caller's responsibility (AC-6.2) — this function does
/// the deleting, the CLI does the asking, so a scripted caller is not fighting
/// a prompt.
pub async fn delete(client: &ContainerdClient, name: &str) -> Result<()> {
    let container_id = resolve(client, name).await?;
    let paths = VolumePaths::from_env()?;

    // 1. Stop the task if one is running. Safe if it is not.
    if client.task_state(&container_id).await?.is_running() {
        client.stop_task(&container_id).await?;
    }

    // 2. Container record and rootfs snapshot.
    client.delete_container(&container_id).await?;

    // 3. Unmount and detach the loop device, via the privileged helper.
    HelperOps::new()
        .unmount_and_detach(name)
        .with_context(|| format!("failed to release the volume for project {name:?}"))?;

    // 4. Backing file and mount point, now that nothing refers to them.
    let image = paths.image_file(name);
    if image.exists() {
        std::fs::remove_file(&image)
            .with_context(|| format!("failed to remove {}", image.display()))?;
    }
    let mount_point = paths.mount_point(name);
    let _ = std::fs::remove_dir(&mount_point);

    Ok(())
}
