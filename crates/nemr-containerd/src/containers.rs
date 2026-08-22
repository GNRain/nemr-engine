//! Container operations.
//!
//! Milestone 1 surface: [`ContainerdClient::list_containers`]. Create, start,
//! stop and delete arrive in Milestones 4–5 and extend this module.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use containerd_client::services::v1::snapshots::{
    ListSnapshotsRequest, MountsRequest, PrepareSnapshotRequest, RemoveSnapshotRequest,
};
use containerd_client::services::v1::{
    container::Runtime, CloseIoRequest, Container, CreateContainerRequest, CreateTaskRequest,
    DeleteContainerRequest, DeleteProcessRequest, DeleteTaskRequest, ExecProcessRequest,
    GetRequest, KillRequest, ListContainersRequest, ResizePtyRequest, StartRequest, WaitRequest,
};
use containerd_client::tonic::{Code, Request};
use containerd_client::with_namespace;
use prost_types::Any;
use tokio::time::timeout;

use super::client::ContainerdClient;
use super::images::ImageConfig;

/// One container, reduced to the fields a consumer actually uses.
///
/// See the note on [`super::images::ImageSummary`] for why this is a
/// projection rather than the generated protobuf type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerSummary {
    /// Container ID, unique within the namespace.
    pub id: String,
    /// Image reference the container was created from.
    pub image: String,
    /// Runtime handling the container, e.g. `io.containerd.runc.v2`.
    pub runtime: String,
    /// Labels stored on the record.
    ///
    /// Included because containerd is where engine metadata lives (see
    /// [`ContainerSpec::labels`]); a caller listing containers needs them to
    /// reconstruct project state without a second round trip or a side
    /// database.
    pub labels: HashMap<String, String>,
}

impl ContainerdClient {
    /// List all containers in this client's namespace.
    ///
    /// Sorted by ID for the same determinism reason as
    /// [`ContainerdClient::list_images`].
    ///
    /// Note this lists *containers* (metadata records), not *tasks* (running
    /// processes). A container with no task is a valid stopped-but-ready
    /// container — the state AC-4.1 requires — so absence of a task is not an
    /// error here.
    pub async fn list_containers(&self) -> Result<Vec<ContainerSummary>> {
        let request = ListContainersRequest { filters: vec![] };

        let response = self
            .raw()
            .containers()
            .list(with_namespace!(request, self.namespace()))
            .await
            .context("containerd ListContainers request failed")?;

        let mut containers: Vec<ContainerSummary> = response
            .into_inner()
            .containers
            .into_iter()
            .map(|container| ContainerSummary {
                id: container.id,
                image: container.image,
                runtime: container.runtime.map(|r| r.name).unwrap_or_default(),
                labels: container.labels,
            })
            .collect();

        containers.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(containers)
    }
}

/// A host path bind-mounted into a container.
#[derive(Debug, Clone)]
pub struct BindMount {
    pub source: PathBuf,
    pub destination: String,
    pub read_only: bool,
}

impl BindMount {
    pub fn read_write(source: impl Into<PathBuf>, destination: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            destination: destination.into(),
            read_only: false,
        }
    }

    pub fn read_only(source: impl Into<PathBuf>, destination: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            destination: destination.into(),
            read_only: true,
        }
    }

    fn options(&self) -> Vec<String> {
        let mut options = vec!["rbind".to_string()];
        options.push(if self.read_only {
            "ro".into()
        } else {
            "rw".into()
        });
        options
    }
}

/// What a container needs to be created.
///
/// Deliberately general: nothing here is specific to a "project". The caller
/// decides what to mount and where; this describes a container, so consumers
/// extend it rather than adding a parallel path (SPEC §3.2).
#[derive(Debug, Clone)]
pub struct ContainerSpec {
    pub id: String,
    pub image: String,
    pub mounts: Vec<BindMount>,
    /// Overrides the image's own working directory when set.
    pub working_dir: Option<String>,
    /// Appended after the image's environment, so these win on duplicates.
    pub extra_env: Vec<String>,
    /// Process to run as PID 1, overriding the image's default command.
    ///
    /// Required by PROC-03: the process model must be explicit in the runtime
    /// spec, so a change to the base image's `CMD` cannot silently alter it.
    /// `None` falls back to the image's own entrypoint + cmd.
    pub args: Option<Vec<String>>,
    /// Name used for the systemd cgroup scope, if it should differ from `id`.
    ///
    /// The scope is named `<prefix>-<name>.scope`, so passing an id that
    /// already carries the prefix yields a doubled name. `None` uses `id`.
    pub cgroup_name: Option<String>,
    /// Prefix for the cgroup slice path, `user.slice:<prefix>:<name>`.
    ///
    /// Product-specific (the caller's, e.g. `"nemr"`), so it lives on the spec
    /// rather than as a wrapper constant — the wrapper stays product-agnostic
    /// and the same crate serves the CLI, the daemon and the connectivity
    /// baselines without carrying anyone's branding.
    pub cgroup_prefix: String,
    /// Labels stored on the container record.
    ///
    /// containerd persists these and returns them from its listing API, so
    /// caller metadata is discoverable through containerd itself rather than a
    /// side database the caller would have to keep in sync.
    pub labels: HashMap<String, String>,
}

impl ContainerdClient {
    /// Whether a container with this ID exists in the namespace.
    pub async fn container_exists(&self, id: &str) -> Result<bool> {
        let request = ListContainersRequest {
            filters: vec![format!("id=={id}")],
        };
        let response = self
            .raw()
            .containers()
            .list(with_namespace!(request, self.namespace()))
            .await
            .context("containerd ListContainers request failed")?;
        Ok(!response.into_inner().containers.is_empty())
    }

    /// Prepare a writable snapshot for a container's rootfs.
    ///
    /// `parent` is the image's rootfs chain ID. containerd performs the
    /// snapshot work server-side and returns the mounts the runtime will later
    /// apply — this crate never mounts anything itself here.
    /// `lease` is **not** optional by accident. A snapshot prepared without one
    /// is unreferenced until a container record names it, and containerd's
    /// collector deletes unreferenced resources — that window is F-63. Making
    /// the parameter explicit forces every call site to state which it wants
    /// rather than inheriting the unsafe default; `gc_probe` passes `None`
    /// deliberately, to demonstrate the failure it exists to demonstrate.
    pub async fn prepare_snapshot(
        &self,
        key: &str,
        parent: &str,
        lease: Option<&crate::leases::Lease>,
    ) -> Result<()> {
        let request = PrepareSnapshotRequest {
            snapshotter: self.snapshotter().to_string(),
            key: key.to_string(),
            parent: parent.to_string(),
            labels: Default::default(),
        };

        self.raw()
            .snapshots()
            .prepare(match lease {
                Some(lease) => {
                    crate::with_lease!(request, self.namespace(), lease.id())
                }
                None => with_namespace!(request, self.namespace()),
            })
            .await
            .with_context(|| {
                format!(
                    "failed to prepare snapshot {key:?} from parent {parent:?} \
                     using snapshotter {:?}",
                    self.snapshotter()
                )
            })?;
        Ok(())
    }

    /// All snapshot keys in this snapshotter, for reconciliation.
    ///
    /// `Snapshots.List` is a server-streaming RPC, so the response is drained
    /// message by message. Used by the orphan sweep to find snapshots that
    /// outlived their container record (a crash between snapshot prepare and
    /// container create, or between task delete and container delete).
    pub async fn list_snapshot_keys(&self) -> Result<Vec<String>> {
        let request = ListSnapshotsRequest {
            snapshotter: self.snapshotter().to_string(),
            filters: vec![],
        };

        let mut stream = self
            .raw()
            .snapshots()
            .list(with_namespace!(request, self.namespace()))
            .await
            .context("containerd Snapshots.List failed")?
            .into_inner();

        let mut keys = Vec::new();
        while let Some(response) = stream
            .message()
            .await
            .context("error draining the snapshot list stream")?
        {
            // `Info.name` is the snapshot key in containerd's snapshots proto.
            keys.extend(response.info.into_iter().map(|info| info.name));
        }
        Ok(keys)
    }

    /// Remove a snapshot. Tolerates absence so cleanup paths can call it
    /// unconditionally.
    pub async fn remove_snapshot(&self, key: &str) -> Result<()> {
        let request = RemoveSnapshotRequest {
            snapshotter: self.snapshotter().to_string(),
            key: key.to_string(),
        };
        match self
            .raw()
            .snapshots()
            .remove(with_namespace!(request, self.namespace()))
            .await
        {
            Ok(_) => Ok(()),
            Err(status) if status.code() == Code::NotFound => Ok(()),
            Err(status) => Err(anyhow::Error::from(status))
                .with_context(|| format!("failed to remove snapshot {key:?}")),
        }
    }

    /// Create a container from an image, with the given mounts.
    ///
    /// Produces a container record and its rootfs snapshot — a stopped,
    /// ready-to-start container. No task is created; starting is Milestone 5.
    ///
    /// On failure after the snapshot exists, the snapshot is removed, so a
    /// failed create leaves nothing behind (NFR-03).
    pub async fn create_container(&self, spec: &ContainerSpec) -> Result<()> {
        let chain_id = self.image_chain_id(&spec.image).await?;
        let image_config = self.image_config(&spec.image).await?;

        // F-63: the lease is acquired BEFORE the snapshot exists and released
        // only after the record that references it has been written. Inside that
        // window the snapshot is attributed to the lease, so containerd's
        // collector leaves it alone; outside it, the container record is the
        // reference and the lease is no longer needed.
        //
        // Without this, the collector could delete the snapshot between the two
        // calls and `create_container_record` would still return Ok — containerd
        // does not validate that `snapshot_key` resolves — producing a container
        // that reports created and can never start.
        let lease = self
            .create_lease(&format!("nemr-create-{}", spec.id))
            .await?;

        let result = async {
            self.prepare_snapshot(&spec.id, &chain_id, Some(&lease))
                .await?;
            // Test seam: lets the F-63 guard run a collection *inside* this
            // window deterministically. `None` everywhere but that test.
            if let Some(hook) = &self.inside_create_window {
                hook().await;
            }
            self.create_container_record(spec, &image_config).await
        }
        .await;

        // Released on BOTH paths. A lease left behind pins its snapshot past the
        // point anything refers to it, which is the opposite leak and a quieter
        // one — nothing fails, the disk just never comes back.
        let released = self.delete_lease(&lease).await;

        if let Err(error) = result {
            // Roll the snapshot back rather than leaving an orphan for a
            // create that did not complete.
            let _ = self.remove_snapshot(&spec.id).await;
            return Err(error);
        }

        // The create succeeded, so a failed release is not fatal — but it is not
        // nothing either: the lease's expiry label bounds the leak to an hour,
        // and saying so beats discovering it from disk usage later.
        if let Err(error) = released {
            tracing::warn!(
                container = %spec.id,
                %error,
                "created the container but could not release its lease; it expires within the hour"
            );
        }
        Ok(())
    }

    async fn create_container_record(
        &self,
        spec: &ContainerSpec,
        image_config: &ImageConfig,
    ) -> Result<()> {
        let oci = oci_spec(spec, image_config);
        let oci_bytes = serde_json::to_vec(&oci).context("failed to serialise the OCI spec")?;

        let container = Container {
            id: spec.id.clone(),
            image: spec.image.clone(),
            runtime: Some(Runtime {
                name: crate::config::RUNTIME.to_string(),
                options: Some(systemd_cgroup_options()),
            }),
            spec: Some(Any {
                type_url: "types.containerd.io/opencontainers/runtime-spec/1/Spec".to_string(),
                value: oci_bytes,
            }),
            snapshotter: self.snapshotter().to_string(),
            snapshot_key: spec.id.clone(),
            labels: spec.labels.clone(),
            ..Default::default()
        };

        let request = CreateContainerRequest {
            container: Some(container),
        };

        self.raw()
            .containers()
            .create(with_namespace!(request, self.namespace()))
            .await
            .with_context(|| format!("failed to create container {:?}", spec.id))?;
        Ok(())
    }

    /// Delete a container record and its rootfs snapshot.
    ///
    /// Tolerates a missing container so it is safe on cleanup paths.
    pub async fn delete_container(&self, id: &str) -> Result<()> {
        let request = DeleteContainerRequest { id: id.to_string() };
        match self
            .raw()
            .containers()
            .delete(with_namespace!(request, self.namespace()))
            .await
        {
            Ok(_) => {}
            Err(status) if status.code() == Code::NotFound => {}
            Err(status) => {
                return Err(anyhow::Error::from(status))
                    .with_context(|| format!("failed to delete container {id:?}"))
            }
        }
        // The snapshot outlives the container record, so remove it explicitly
        // or it becomes an orphan (NFR-03).
        self.remove_snapshot(id).await
    }
}

/// Build an OCI runtime spec.
///
/// Modelled on the spec containerd itself generates for this image, captured
/// from `ctr containers info` rather than written from the OCI document —
/// containerd's defaults are what runc is actually exercised against here, and
/// guessing at them produces containers that fail at start in obscure ways.
///
/// Note there is no `user` namespace entry: under PRIV-01 the container
/// inherits rootlesskit's existing user namespace. Adding one here would ask
/// runc to nest a second namespace and require its own uid mappings.
fn oci_spec(spec: &ContainerSpec, image_config: &ImageConfig) -> serde_json::Value {
    let mut env = image_config.env.clone();
    env.extend(spec.extra_env.iter().cloned());

    let cwd = spec
        .working_dir
        .clone()
        .or_else(|| image_config.working_dir.clone())
        .unwrap_or_else(|| "/".to_string());

    let mut mounts = default_mounts();
    for bind in &spec.mounts {
        mounts.push(serde_json::json!({
            "destination": bind.destination,
            "type": "bind",
            "source": bind.source.to_string_lossy(),
            "options": bind.options(),
        }));
    }

    serde_json::json!({
        "ociVersion": "1.3.0",
        "process": {
            "terminal": false,
            "user": { "uid": 0, "gid": 0, "additionalGids": [0] },
            "args": spec.args.clone().unwrap_or_else(|| image_config.args()),
            "env": env,
            "cwd": cwd,
            "capabilities": {
                "bounding":    DEFAULT_CAPABILITIES,
                "effective":   DEFAULT_CAPABILITIES,
                "permitted":   DEFAULT_CAPABILITIES,
            },
            "rlimits": [ { "type": "RLIMIT_NOFILE", "hard": 1024, "soft": 1024 } ],
            "noNewPrivileges": true
        },
        "root": { "path": "rootfs" },
        "mounts": mounts,
        "linux": {
            "resources": { "devices": [ { "allow": false, "access": "rwm" } ] },
            // systemd driver form, "slice:prefix:name". Under PRIV-01 runc's
            // default path (/{namespace}/{id}) is unwritable: containerd inside
            // rootlesskit still sees the host /sys/fs/cgroup, so creating a
            // cgroup at the root fails with EPERM. Placing the task in a scope
            // under the delegated user slice is what works.
            "cgroupsPath": format!(
                "user.slice:{}:{}",
                spec.cgroup_prefix,
                spec.cgroup_name.as_deref().unwrap_or(&spec.id)
            ),
            "namespaces": [
                { "type": "pid" }, { "type": "ipc" }, { "type": "uts" },
                { "type": "mount" }
            ],
            "maskedPaths": [
                "/proc/acpi", "/proc/asound", "/proc/kcore", "/proc/keys",
                "/proc/latency_stats", "/proc/timer_list", "/proc/timer_stats",
                "/proc/sched_debug", "/sys/firmware", "/proc/scsi"
            ],
            "readonlyPaths": [
                "/proc/bus", "/proc/fs", "/proc/irq", "/proc/sys", "/proc/sysrq-trigger"
            ]
        }
    })
}

/// containerd's default capability set (14 capabilities).
const DEFAULT_CAPABILITIES: [&str; 14] = [
    "CAP_CHOWN",
    "CAP_DAC_OVERRIDE",
    "CAP_FSETID",
    "CAP_FOWNER",
    "CAP_MKNOD",
    "CAP_NET_RAW",
    "CAP_SETGID",
    "CAP_SETUID",
    "CAP_SETFCAP",
    "CAP_SETPCAP",
    "CAP_NET_BIND_SERVICE",
    "CAP_SYS_CHROOT",
    "CAP_KILL",
    "CAP_AUDIT_WRITE",
];

/// The standard filesystem mounts every container gets.
fn default_mounts() -> Vec<serde_json::Value> {
    serde_json::json!([
        { "destination": "/proc", "type": "proc", "source": "proc",
          "options": ["nosuid", "noexec", "nodev"] },
        { "destination": "/dev", "type": "tmpfs", "source": "tmpfs",
          "options": ["nosuid", "strictatime", "mode=755", "size=65536k"] },
        { "destination": "/dev/pts", "type": "devpts", "source": "devpts",
          "options": ["nosuid", "noexec", "newinstance", "ptmxmode=0666", "mode=0620", "gid=5"] },
        { "destination": "/dev/shm", "type": "tmpfs", "source": "shm",
          "options": ["nosuid", "noexec", "nodev", "mode=1777", "size=65536k"] },
        { "destination": "/dev/mqueue", "type": "mqueue", "source": "mqueue",
          "options": ["nosuid", "noexec", "nodev"] },
        { "destination": "/sys", "type": "sysfs", "source": "sysfs",
          "options": ["nosuid", "noexec", "nodev", "ro"] },
        { "destination": "/run", "type": "tmpfs", "source": "tmpfs",
          "options": ["nosuid", "strictatime", "mode=755", "size=65536k"] }
    ])
    .as_array()
    .cloned()
    .unwrap_or_default()
}

/// Runtime options selecting runc's systemd cgroup driver.
///
/// `containerd-client` does not generate the `containerd.runc.v1.Options`
/// type: the proto ships in the crate's `vendor/` directory but is absent from
/// its `build.rs` compile list, so there is no Rust struct to populate.
///
/// The message is therefore encoded by hand. It is a single field —
/// `bool systemd_cgroup = 9` — so the encoding is tag `(9 << 3) | 0 = 0x48`
/// followed by varint `0x01`.
///
/// These exact bytes were not derived and hoped for: `ctr` was asked to create
/// a container with `--runc-systemd-cgroup`, and the options it sent were read
/// back off the container record. They were `type_url =
/// "containerd.runc.v1.Options"`, `value = [0x48, 0x01]`. Worth noting the
/// field number is 9, not the 5 its position in the proto might suggest — the
/// numbering is not contiguous, and a wrong guess encodes cleanly while
/// setting an entirely different option.
fn systemd_cgroup_options() -> Any {
    Any {
        type_url: "containerd.runc.v1.Options".to_string(),
        value: vec![0x48, 0x01],
    }
}

/// Lifecycle state of a container's task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    /// No task exists — the container is created but not started.
    None,
    Created,
    Running,
    Stopped,
    Paused,
    Unknown,
}

impl TaskState {
    pub fn is_running(self) -> bool {
        matches!(self, Self::Running | Self::Created)
    }
}

/// How a task actually came to a stop.
///
/// Exists because "the task is stopped" is not the same claim as "the task shut
/// down cleanly", and conflating them hid PROC-06 for the whole of Phase 1: a
/// supervisor that ignored SIGTERM was killed after a five-second timeout on
/// every single stop, and `stop` reported success either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopOutcome {
    /// The task handled SIGTERM and exited within the grace period.
    Graceful,
    /// The task ignored SIGTERM and had to be killed. Not an error, but it
    /// means the container got no chance to flush or shut down cleanly, and it
    /// is worth surfacing rather than swallowing.
    Killed,
    /// There was no task to stop.
    NoTask,
}

impl std::fmt::Display for StopOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Graceful => "terminated gracefully",
            Self::Killed => "ignored SIGTERM; killed after the grace period",
            Self::NoTask => "no task was running",
        })
    }
}

impl ContainerdClient {
    /// Current task state for a container.
    ///
    /// A missing task is [`TaskState::None`] rather than an error: "created but
    /// never started" is a legitimate state (AC-4.1), and callers need to
    /// distinguish it from a failure to ask.
    pub async fn task_state(&self, id: &str) -> Result<TaskState> {
        let request = GetRequest {
            container_id: id.to_string(),
            exec_id: String::new(),
        };

        match self
            .raw()
            .tasks()
            .get(with_namespace!(request, self.namespace()))
            .await
        {
            Ok(response) => {
                let status = response.into_inner().process.map(|p| p.status);
                // Values from containerd.v1.types.Status.
                Ok(match status {
                    Some(1) => TaskState::Created,
                    Some(2) => TaskState::Running,
                    Some(3) => TaskState::Stopped,
                    Some(4) => TaskState::Paused,
                    Some(_) | None => TaskState::Unknown,
                })
            }
            Err(status) if status.code() == Code::NotFound => Ok(TaskState::None),
            Err(status) => Err(anyhow::Error::from(status))
                .with_context(|| format!("failed to query task state for {id:?}")),
        }
    }

    /// Rootfs mounts for a container's snapshot.
    ///
    /// Task creation needs these: containerd does not infer them from the
    /// container's snapshot key, the client supplies them.
    async fn snapshot_mounts(&self, key: &str) -> Result<Vec<containerd_client::types::Mount>> {
        let request = MountsRequest {
            snapshotter: self.snapshotter().to_string(),
            key: key.to_string(),
        };

        let response = self
            .raw()
            .snapshots()
            .mounts(with_namespace!(request, self.namespace()))
            .await
            .with_context(|| format!("failed to get mounts for snapshot {key:?}"))?;

        Ok(response.into_inner().mounts)
    }

    /// Start a container's task (PID 1).
    ///
    /// IO is left unattached: PID 1 is a supervisor (PROC-01), not something a
    /// user talks to. Interactive sessions are separate execs with their own
    /// terminals (PROC-02).
    pub async fn start_task(&self, id: &str) -> Result<u32> {
        let mounts = self.snapshot_mounts(id).await?;

        let create = CreateTaskRequest {
            container_id: id.to_string(),
            rootfs: mounts,
            terminal: false,
            stdin: String::new(),
            stdout: String::new(),
            stderr: String::new(),
            ..Default::default()
        };

        self.raw()
            .tasks()
            .create(with_namespace!(create, self.namespace()))
            .await
            .with_context(|| format!("failed to create task for container {id:?}"))?;

        let start = StartRequest {
            container_id: id.to_string(),
            exec_id: String::new(),
        };

        let response = self
            .raw()
            .tasks()
            .start(with_namespace!(start, self.namespace()))
            .await
            .with_context(|| format!("failed to start task for container {id:?}"))?;

        Ok(response.into_inner().pid)
    }

    /// Stop a container's task: signal it, wait for exit, then delete it.
    ///
    /// Deleting the task is what returns the container to the stopped-but-ready
    /// state of AC-4.1 (PROC-04). Leaving a stopped-but-undeleted task behind
    /// would make a later `start` fail with "already exists".
    ///
    /// # Why the outcome is reported rather than discarded
    ///
    /// Per `pid_namespaces(7)`, the kernel delivers a signal sent from an
    /// ancestor namespace to a namespace's PID 1 **only if that process has
    /// installed a handler for it** — SIGKILL and SIGSTOP being the exceptions.
    /// A supervisor that traps nothing therefore never sees SIGTERM at all, and
    /// `stop` silently degrades into "wait out the grace period, then SIGKILL".
    ///
    /// That degradation is invisible from the outside: the task does stop, and
    /// the command does report success. It cost this project a defect (PROC-06)
    /// that survived precisely because nothing distinguished the two paths. So
    /// the path taken is returned rather than dropped, and callers surface it.
    pub async fn stop_task(&self, id: &str) -> Result<StopOutcome> {
        const GRACE: Duration = Duration::from_secs(5);
        const KILL_TIMEOUT: Duration = Duration::from_secs(10);

        // Nothing to stop. Reported distinctly so a caller can tell "already
        // stopped" from "stopped by us", rather than inferring it.
        if self.task_state(id).await? == TaskState::None {
            return Ok(StopOutcome::NoTask);
        }

        self.signal_task(id, 15).await?;

        let outcome = if timeout(GRACE, self.wait_task(id)).await.is_err() {
            self.signal_task(id, 9).await?;
            timeout(KILL_TIMEOUT, self.wait_task(id))
                .await
                .with_context(|| {
                    format!("task for {id:?} did not exit within {KILL_TIMEOUT:?} of SIGKILL")
                })??;
            StopOutcome::Killed
        } else {
            StopOutcome::Graceful
        };

        let delete = DeleteTaskRequest {
            container_id: id.to_string(),
        };
        match self
            .raw()
            .tasks()
            .delete(with_namespace!(delete, self.namespace()))
            .await
        {
            Ok(_) => Ok(outcome),
            Err(status) if status.code() == Code::NotFound => Ok(outcome),
            Err(status) => Err(anyhow::Error::from(status))
                .with_context(|| format!("failed to delete task for {id:?}")),
        }
    }

    /// Send a signal to a task's whole process group. A missing task is not an
    /// error, so cleanup paths can call this unconditionally.
    async fn signal_task(&self, id: &str, signal: u32) -> Result<()> {
        let kill = KillRequest {
            container_id: id.to_string(),
            exec_id: String::new(),
            signal,
            all: true,
        };
        match self
            .raw()
            .tasks()
            .kill(with_namespace!(kill, self.namespace()))
            .await
        {
            Ok(_) => Ok(()),
            Err(status) if status.code() == Code::NotFound => Ok(()),
            Err(status) => Err(anyhow::Error::from(status))
                .with_context(|| format!("failed to send signal {signal} to task {id:?}")),
        }
    }

    /// Block until a task exits.
    async fn wait_task(&self, id: &str) -> Result<()> {
        let wait = WaitRequest {
            container_id: id.to_string(),
            exec_id: String::new(),
        };
        match self
            .raw()
            .tasks()
            .wait(with_namespace!(wait, self.namespace()))
            .await
        {
            Ok(_) => Ok(()),
            Err(status) if status.code() == Code::NotFound => Ok(()),
            Err(status) => Err(anyhow::Error::from(status))
                .with_context(|| format!("failed waiting for task {id:?}")),
        }
    }
}

/// How an exec's IO is wired up.
#[derive(Debug, Clone)]
pub struct ExecIo {
    /// FIFO the caller writes the process's stdin into.
    pub stdin: PathBuf,
    /// FIFO the caller reads the process's output from.
    pub stdout: PathBuf,
    /// FIFO for stderr. Only used without a terminal — a pty merges the two
    /// streams, and containerd rejects a spec that sets both.
    pub stderr: Option<PathBuf>,
    /// Allocate a pseudo-terminal for the process.
    pub terminal: bool,
}

impl ContainerdClient {
    /// Run a process inside a container's existing task.
    ///
    /// This is how an interactive session is obtained (PROC-02): a fresh
    /// process with its own terminal, independent of PID 1. Several execs can
    /// coexist; each has its own `exec_id` and its own IO.
    ///
    /// The FIFOs must already exist — containerd's shim opens them, it does not
    /// create them. They are opened when the process starts, so create them
    /// before calling and open them O_RDWR to avoid the blocking-open deadlock.
    ///
    /// With `terminal: true` no stderr FIFO is passed: a pty merges the two
    /// streams, and containerd rejects a spec that sets both.
    pub async fn exec_process(
        &self,
        container_id: &str,
        exec_id: &str,
        process: serde_json::Value,
        io: &ExecIo,
    ) -> Result<()> {
        let spec_bytes =
            serde_json::to_vec(&process).context("failed to serialise the exec process spec")?;

        let request = ExecProcessRequest {
            container_id: container_id.to_string(),
            exec_id: exec_id.to_string(),
            terminal: io.terminal,
            stdin: io.stdin.to_string_lossy().to_string(),
            stdout: io.stdout.to_string_lossy().to_string(),
            stderr: io
                .stderr
                .as_ref()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default(),
            spec: Some(Any {
                type_url: "types.containerd.io/opencontainers/runtime-spec/1/Process".to_string(),
                value: spec_bytes,
            }),
        };

        self.raw()
            .tasks()
            .exec(with_namespace!(request, self.namespace()))
            .await
            .with_context(|| {
                format!("failed to create exec {exec_id:?} in container {container_id:?}")
            })?;
        Ok(())
    }

    /// Start a previously created exec process.
    pub async fn start_exec(&self, container_id: &str, exec_id: &str) -> Result<u32> {
        let request = StartRequest {
            container_id: container_id.to_string(),
            exec_id: exec_id.to_string(),
        };
        let response = self
            .raw()
            .tasks()
            .start(with_namespace!(request, self.namespace()))
            .await
            .with_context(|| format!("failed to start exec {exec_id:?}"))?;
        Ok(response.into_inner().pid)
    }

    /// Resize an exec's pseudo-terminal.
    ///
    /// Without this the process believes the terminal is whatever size it was
    /// at creation, so full-screen output wraps wrongly after a window resize.
    pub async fn resize_pty(
        &self,
        container_id: &str,
        exec_id: &str,
        width: u32,
        height: u32,
    ) -> Result<()> {
        let request = ResizePtyRequest {
            container_id: container_id.to_string(),
            exec_id: exec_id.to_string(),
            width,
            height,
        };
        self.raw()
            .tasks()
            .resize_pty(with_namespace!(request, self.namespace()))
            .await
            .with_context(|| format!("failed to resize pty for exec {exec_id:?}"))?;
        Ok(())
    }

    /// Wait for an exec to exit, returning its exit code.
    pub async fn wait_exec(&self, container_id: &str, exec_id: &str) -> Result<u32> {
        let request = WaitRequest {
            container_id: container_id.to_string(),
            exec_id: exec_id.to_string(),
        };
        let response = self
            .raw()
            .tasks()
            .wait(with_namespace!(request, self.namespace()))
            .await
            .with_context(|| format!("failed waiting for exec {exec_id:?}"))?;
        Ok(response.into_inner().exit_status)
    }

    /// Signal that no more stdin will be written.
    pub async fn close_exec_stdin(&self, container_id: &str, exec_id: &str) -> Result<()> {
        let request = CloseIoRequest {
            container_id: container_id.to_string(),
            exec_id: exec_id.to_string(),
            stdin: true,
        };
        let _ = self
            .raw()
            .tasks()
            .close_io(with_namespace!(request, self.namespace()))
            .await;
        Ok(())
    }

    /// Delete an exec's process record.
    ///
    /// Execs are not reaped automatically; leaving them accumulates process
    /// records on the task (NFR-03).
    pub async fn delete_exec(&self, container_id: &str, exec_id: &str) -> Result<()> {
        let request = DeleteProcessRequest {
            container_id: container_id.to_string(),
            exec_id: exec_id.to_string(),
        };
        match self
            .raw()
            .tasks()
            .delete_process(with_namespace!(request, self.namespace()))
            .await
        {
            Ok(_) => Ok(()),
            Err(status) if status.code() == Code::NotFound => Ok(()),
            Err(status) => Err(anyhow::Error::from(status))
                .with_context(|| format!("failed to delete exec {exec_id:?}")),
        }
    }
}
