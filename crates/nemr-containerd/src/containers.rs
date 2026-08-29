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
    StatSnapshotRequest,
};
use containerd_client::services::v1::{
    container::Runtime, CloseIoRequest, Container, CreateContainerRequest, CreateTaskRequest,
    DeleteContainerRequest, DeleteProcessRequest, DeleteTaskRequest, ExecProcessRequest,
    GetRequest, KillRequest, ListContainersRequest, ResizePtyRequest, StartRequest,
    UpdateContainerRequest, WaitRequest,
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

/// Add a network namespace to an OCI spec if it does not already ask for one.
///
/// Returns whether it changed anything, so the caller can avoid a pointless
/// containerd write and can report a real migration to the user. Pure, and
/// therefore testable against a spec captured from a container that predates
/// NET-02 — which is the only shape that matters here and the one no freshly
/// created container can produce.
pub fn add_network_namespace(oci: &mut serde_json::Value) -> Result<bool> {
    let namespaces = oci
        .get_mut("linux")
        .and_then(|l| l.get_mut("namespaces"))
        .and_then(|n| n.as_array_mut())
        .context("the OCI spec has no linux.namespaces array")?;

    if namespaces
        .iter()
        .any(|n| n.get("type").and_then(|t| t.as_str()) == Some("network"))
    {
        return Ok(false);
    }
    namespaces.push(serde_json::json!({ "type": "network" }));
    Ok(true)
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
    /// Ask for a private network namespace (NET-02). Always true in production.
    ///
    /// It is a field rather than a constant because the OTHER shape genuinely
    /// exists on every machine that ran this engine before NET-02: those
    /// container records are frozen without it, and the migration that repairs
    /// them is the upgrade path every existing user takes. A test cannot cover
    /// that path without being able to build the shape it repairs.
    pub own_network_namespace: bool,
}

impl Default for ContainerSpec {
    fn default() -> Self {
        Self {
            id: String::new(),
            image: String::new(),
            mounts: Vec::new(),
            working_dir: None,
            extra_env: Vec::new(),
            args: None,
            cgroup_name: None,
            cgroup_prefix: String::new(),
            labels: HashMap::new(),
            own_network_namespace: true,
        }
    }
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

    /// Update the labels on an existing container record.
    ///
    /// Used to change a project's recorded agent (E-15). Sends an
    /// `UpdateContainerRequest` with a `labels` field mask, so only the labels
    /// are touched — the snapshot, runtime and OCI spec are left exactly as they
    /// were. containerd requires the target container to carry the id.
    pub async fn update_container_labels(
        &self,
        id: &str,
        labels: std::collections::HashMap<String, String>,
    ) -> Result<()> {
        let request = UpdateContainerRequest {
            container: Some(Container {
                id: id.to_string(),
                labels,
                ..Default::default()
            }),
            update_mask: Some(prost_types::FieldMask {
                paths: vec!["labels".to_string()],
            }),
        };

        self.raw()
            .containers()
            .update(with_namespace!(request, self.namespace()))
            .await
            .with_context(|| format!("failed to update labels on container {id:?}"))?;
        Ok(())
    }

    /// Give a container its own network namespace if its stored OCI spec does
    /// not already ask for one (NET-02 migration). Returns whether the record
    /// was changed.
    ///
    /// # Why this exists
    ///
    /// The OCI spec is frozen into the container record at CREATE time and read
    /// again at every task start. So a project created before NET-02 keeps a
    /// spec with `namespaces: [pid, ipc, uts, mount]` for ever, and its task
    /// joins rootlesskit's network namespace exactly as it always did — no
    /// amount of new engine code changes that. Recording an allocation on such a
    /// project and then wiring it, as the first NET-02 migration did, tries to
    /// build a session network inside the SHARED namespace: the veth is created,
    /// the addresses land in rootlesskit's own namespace, and
    /// `ip route add default` fails with "RTNETLINK answers: File exists"
    /// because rootlesskit already has one. That is a project that used to start
    /// and no longer does.
    ///
    /// The edit is deliberately minimal — one entry appended to
    /// `linux.namespaces`, every other byte of the spec left as it was. A
    /// project that has travelled between machines carries mounts, a cgroup path
    /// and process arguments in that spec, and regenerating it from today's
    /// inputs could change any of them.
    pub async fn ensure_own_network_namespace(&self, id: &str) -> Result<bool> {
        let request = ListContainersRequest {
            filters: vec![format!("id=={id}")],
        };
        // (read-only sibling: `has_own_network_namespace`)
        let mut containers = self
            .raw()
            .containers()
            .list(with_namespace!(request, self.namespace()))
            .await
            .context("containerd ListContainers request failed")?
            .into_inner()
            .containers;
        let Some(container) = containers.pop() else {
            anyhow::bail!("container {id:?} does not exist");
        };
        let Some(spec) = container.spec.clone() else {
            anyhow::bail!("container {id:?} has no OCI spec recorded");
        };

        let mut oci: serde_json::Value = serde_json::from_slice(&spec.value)
            .with_context(|| format!("parsing the OCI spec recorded for {id:?}"))?;

        if !add_network_namespace(&mut oci)
            .with_context(|| format!("reading linux.namespaces from the spec for {id:?}"))?
        {
            return Ok(false);
        }

        let updated = serde_json::to_vec(&oci).context("re-serialising the OCI spec")?;
        let request = UpdateContainerRequest {
            container: Some(Container {
                id: id.to_string(),
                spec: Some(Any {
                    type_url: spec.type_url,
                    value: updated,
                }),
                ..Default::default()
            }),
            update_mask: Some(prost_types::FieldMask {
                paths: vec!["spec".to_string()],
            }),
        };
        self.raw()
            .containers()
            .update(with_namespace!(request, self.namespace()))
            .await
            .with_context(|| {
                format!("failed to add a network namespace to the OCI spec of {id:?}")
            })?;
        Ok(true)
    }

    /// Does this container's stored OCI spec ask for its own network namespace?
    ///
    /// Read-only, deliberately. A test that established the same fact by calling
    /// `ensure_own_network_namespace` performed the migration it was checking
    /// for, so the subject was already repaired before the code under test ran —
    /// and the test stayed green with the migration deleted. A control that
    /// changes what it observes is not a control.
    pub async fn has_own_network_namespace(&self, id: &str) -> Result<bool> {
        let request = ListContainersRequest {
            filters: vec![format!("id=={id}")],
        };
        let mut containers = self
            .raw()
            .containers()
            .list(with_namespace!(request, self.namespace()))
            .await
            .context("containerd ListContainers request failed")?
            .into_inner()
            .containers;
        let Some(container) = containers.pop() else {
            anyhow::bail!("container {id:?} does not exist");
        };
        let spec = container
            .spec
            .with_context(|| format!("container {id:?} has no OCI spec recorded"))?;
        let oci: serde_json::Value = serde_json::from_slice(&spec.value)
            .with_context(|| format!("parsing the OCI spec recorded for {id:?}"))?;
        Ok(oci
            .get("linux")
            .and_then(|l| l.get("namespaces"))
            .and_then(|n| n.as_array())
            .context("the OCI spec has no linux.namespaces array")?
            .iter()
            .any(|n| n.get("type").and_then(|t| t.as_str()) == Some("network")))
    }

    /// The parent of a container's rootfs snapshot — the CHAIN ID of the image
    /// it was actually built from.
    ///
    /// This is the only durable answer to "what does this project run on".
    /// `container.image` is a reference that may have been retagged, may name an
    /// image that is no longer present, and on this project's own history names
    /// a squattable registry namespace (F-25). The chain id is fixed when the
    /// snapshot is prepared at create time — `create_container` passes exactly
    /// this value to `prepare_snapshot` — and it stays readable long after the
    /// image itself has been removed.
    pub async fn snapshot_parent(&self, key: &str) -> Result<String> {
        let request = StatSnapshotRequest {
            snapshotter: self.snapshotter().to_string(),
            key: key.to_string(),
        };
        let info = self
            .raw()
            .snapshots()
            .stat(with_namespace!(request, self.namespace()))
            .await
            .with_context(|| format!("containerd Snapshots.Stat failed for {key:?}"))?
            .into_inner()
            .info
            .with_context(|| format!("snapshot {key:?} exists but carries no info"))?;
        Ok(info.parent)
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
    let mut namespaces = vec![
        serde_json::json!({ "type": "pid" }),
        serde_json::json!({ "type": "ipc" }),
        serde_json::json!({ "type": "uts" }),
        serde_json::json!({ "type": "mount" }),
    ];
    if spec.own_network_namespace {
        namespaces.push(serde_json::json!({ "type": "network" }));
    }
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
            // NET-02: each session gets its OWN network namespace, so two
            // sessions can both bind port 8000 internally — the normal
            // expectation, and what Docker does. A fresh netns has only a down
            // loopback; the engine wires a veth pair and NAT into it
            // immediately after start (engine::netns). Before NET-02 this entry
            // was absent, so sessions shared rootlesskit's namespace (NET-01)
            // and the second bind of any port simply failed — and records
            // created then still carry that shape, which is what
            // `ensure_own_network_namespace` repairs.
            "namespaces": namespaces,
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
    /// SIGKILL was sent but the task did not exit within the window — it is in
    /// an uninterruptible wait (F-78: a task in `D` state, e.g. inside
    /// `kernel_clone`, cannot be reaped until the kernel operation returns).
    /// Distinct from every other outcome ON PURPOSE: it is neither a success
    /// nor a generic error but a specific, surfaced condition — the task may
    /// still be running, and a caller must say so, never claim the stop worked.
    Wedged,
}

impl std::fmt::Display for StopOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Graceful => "terminated gracefully",
            Self::Killed => "ignored SIGTERM; killed after the grace period",
            Self::NoTask => "no task was running",
            Self::Wedged => "did NOT stop: wedged in uninterruptible sleep (SIGKILL cannot                              reap it until the kernel operation it is blocked in returns)",
        })
    }
}

impl StopOutcome {
    /// The stable identifier crossing the daemon protocol (E-09). Never change
    /// these — the CLI branches on them.
    pub fn wire_str(self) -> &'static str {
        match self {
            Self::Graceful => "graceful",
            Self::Killed => "killed",
            Self::NoTask => "no_task",
            Self::Wedged => "wedged",
        }
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
    /// The overlay directories of a snapshot: (upperdir, lowerdirs in overlay
    /// order).
    ///
    /// Derived from the same Snapshots.Mounts call `start_task` uses, so it
    /// describes exactly the filesystem a task of this container would see.
    /// The paths are relative to the MOUNT NAMESPACE containerd runs in — under
    /// rootless containerd that is rootlesskit's, not the host's — so a caller
    /// on the host must read them through `nsenter`, never directly. `nemrd`
    /// runs in the host mount namespace; forgetting the hop yields "permission
    /// denied" or, worse, an EMPTY directory that reads as a clean negative.
    pub async fn snapshot_overlay_dirs(&self, key: &str) -> Result<(String, Vec<String>)> {
        let mounts = self.snapshot_mounts(key).await?;
        let mount = mounts
            .first()
            .with_context(|| format!("snapshot {key:?} has no mounts"))?;
        let mut upper = None;
        let mut lowers = Vec::new();
        for option in &mount.options {
            if let Some(dir) = option.strip_prefix("upperdir=") {
                upper = Some(dir.to_string());
            } else if let Some(dirs) = option.strip_prefix("lowerdir=") {
                lowers = dirs.split(':').map(str::to_string).collect();
            }
        }
        let upper = upper.with_context(|| {
            format!(
                "snapshot {key:?} has no upperdir — its mount type is {:?}, not overlay. \
                 The overlayfs snapshotter is the only one this engine configures.",
                mount.r#type
            )
        })?;
        Ok((upper, lowers))
    }

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
        const GRACE: Duration = crate::config::SIGTERM_GRACE;
        const KILL_TIMEOUT: Duration = Duration::from_secs(10);

        // Nothing to stop. Reported distinctly so a caller can tell "already
        // stopped" from "stopped by us", rather than inferring it.
        if self.task_state(id).await? == TaskState::None {
            return Ok(StopOutcome::NoTask);
        }

        self.signal_task(id, 15).await?;

        let outcome = if timeout(GRACE, self.wait_task(id)).await.is_err() {
            self.signal_task(id, 9).await?;
            // F-78: a task inside an uninterruptible kernel wait cannot be
            // reaped by SIGKILL until that operation returns. Classify that as
            // Wedged and RETURN — do not error generically, and do not fall
            // through to delete the task record (the process is still there).
            // Caution, also from F-78: under CPU starvation tokio's timer fires
            // late, so this branch may be reached well after KILL_TIMEOUT of
            // wall-clock; the classification is still correct, the timing is
            // not to be trusted as elapsed time.
            match timeout(KILL_TIMEOUT, self.wait_task(id)).await {
                Ok(result) => {
                    result?;
                    StopOutcome::Killed
                }
                Err(_) => {
                    tracing::warn!(
                        "[nemr] task for {id:?} did not exit within {KILL_TIMEOUT:?} of SIGKILL                          — wedged in uninterruptible sleep (F-78)"
                    );
                    return Ok(StopOutcome::Wedged);
                }
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact `linux.namespaces` a container created before NET-02 carries.
    /// Read off a real record on 2026-08-29 (`nemr-htmltest`, created
    /// 2026-08-23): a spec frozen at create time and never revisited, which is
    /// why the migration exists at all.
    fn pre_net02_spec() -> serde_json::Value {
        serde_json::json!({
            "ociVersion": "1.0.2-dev",
            "linux": {
                "namespaces": [
                    { "type": "pid" }, { "type": "ipc" },
                    { "type": "uts" }, { "type": "mount" }
                ]
            }
        })
    }

    #[test]
    fn a_pre_net02_spec_gains_a_network_namespace() {
        let mut oci = pre_net02_spec();
        assert!(
            add_network_namespace(&mut oci).unwrap(),
            "a spec with no network namespace must be changed"
        );
        let types: Vec<&str> = oci["linux"]["namespaces"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["type"].as_str().unwrap())
            .collect();
        assert_eq!(types, ["pid", "ipc", "uts", "mount", "network"]);
    }

    /// Idempotent, and it says so — the caller reports a migration to the user
    /// and must not report one on every start for ever after.
    #[test]
    fn migrating_a_spec_twice_changes_nothing_the_second_time() {
        let mut oci = pre_net02_spec();
        assert!(add_network_namespace(&mut oci).unwrap());
        let after_first = oci.clone();
        assert!(
            !add_network_namespace(&mut oci).unwrap(),
            "the second call must report no change"
        );
        assert_eq!(oci, after_first, "and must not change the spec either");
    }

    /// Nothing outside `linux.namespaces` may move. These records carry mounts,
    /// a cgroup path and process arguments for projects that have travelled
    /// between machines; regenerating the spec from today's inputs could change
    /// any of them, so the migration is additive and this pins that.
    #[test]
    fn the_migration_touches_nothing_but_the_namespace_list() {
        let mut oci = serde_json::json!({
            "ociVersion": "1.0.2-dev",
            "process": { "args": ["/usr/local/bin/nemr-supervisor"], "cwd": "/workspace" },
            "mounts": [{ "destination": "/workspace", "source": "/host/vol" }],
            "linux": {
                "cgroupsPath": "user.slice:nemr:demo",
                "namespaces": [{ "type": "pid" }, { "type": "mount" }]
            }
        });
        let before = oci.clone();
        assert!(add_network_namespace(&mut oci).unwrap());

        assert_eq!(oci["process"], before["process"]);
        assert_eq!(oci["mounts"], before["mounts"]);
        assert_eq!(oci["ociVersion"], before["ociVersion"]);
        assert_eq!(oci["linux"]["cgroupsPath"], before["linux"]["cgroupsPath"]);
    }

    /// A spec that is not shaped like one must not be silently "migrated" into
    /// something else. Refusing names the problem; adding a namespaces array to
    /// a spec that has none would write a record runc cannot use.
    #[test]
    fn a_spec_without_a_namespace_list_is_refused_rather_than_invented() {
        let mut oci = serde_json::json!({ "ociVersion": "1.0.2-dev" });
        assert!(add_network_namespace(&mut oci).is_err());
    }

    /// The two shapes the builder can produce. `own_network_namespace` exists so
    /// a test can construct the pre-NET-02 one; nothing in production sets it
    /// false, and this pins both directions.
    #[test]
    fn the_spec_builder_emits_the_network_namespace_only_when_asked() {
        let image = ImageConfig::default();
        let base = ContainerSpec {
            id: "demo".into(),
            image: "img".into(),
            cgroup_prefix: "nemr".into(),
            ..Default::default()
        };

        let with = oci_spec(&base, &image);
        let types: Vec<String> = with["linux"]["namespaces"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["type"].as_str().unwrap().to_string())
            .collect();
        assert!(types.contains(&"network".to_string()));

        let without = oci_spec(
            &ContainerSpec {
                own_network_namespace: false,
                ..base
            },
            &image,
        );
        let types: Vec<String> = without["linux"]["namespaces"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["type"].as_str().unwrap().to_string())
            .collect();
        assert!(
            !types.contains(&"network".to_string()),
            "this is the shape every pre-NET-02 record has, and the migration's subject"
        );
    }
}
