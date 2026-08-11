//! Container operations.
//!
//! Milestone 1 surface: [`ContainerdClient::list_containers`]. Create, start,
//! stop and delete arrive in Milestones 4–5 and extend this module.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use containerd_client::services::v1::snapshots::{PrepareSnapshotRequest, RemoveSnapshotRequest};
use containerd_client::services::v1::{
    container::Runtime, Container, CreateContainerRequest, DeleteContainerRequest,
    ListContainersRequest,
};
use containerd_client::tonic::{Code, Request};
use containerd_client::with_namespace;
use prost_types::Any;

use super::client::ContainerdClient;
use super::images::ImageConfig;

/// One container, reduced to the fields the engine actually uses.
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
        Self { source: source.into(), destination: destination.into(), read_only: false }
    }

    pub fn read_only(source: impl Into<PathBuf>, destination: impl Into<String>) -> Self {
        Self { source: source.into(), destination: destination.into(), read_only: true }
    }

    fn options(&self) -> Vec<String> {
        let mut options = vec!["rbind".to_string()];
        options.push(if self.read_only { "ro".into() } else { "rw".into() });
        options
    }
}

/// What a container needs to be created.
///
/// Deliberately general: nothing here is specific to a "project". The engine
/// layer decides what to mount and where; this describes a container, so later
/// milestones extend it rather than adding a parallel path (Section 3.2).
#[derive(Debug, Clone)]
pub struct ContainerSpec {
    pub id: String,
    pub image: String,
    pub mounts: Vec<BindMount>,
    /// Overrides the image's own working directory when set.
    pub working_dir: Option<String>,
    /// Appended after the image's environment, so these win on duplicates.
    pub extra_env: Vec<String>,
    /// Labels stored on the container record.
    ///
    /// containerd persists these and returns them from its listing API, so
    /// engine metadata (which project, which volume, what quota) is
    /// discoverable through containerd itself rather than a side database the
    /// engine would have to keep in sync.
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
    /// apply — the engine never mounts anything itself here.
    pub async fn prepare_snapshot(&self, key: &str, parent: &str) -> Result<()> {
        let request = PrepareSnapshotRequest {
            snapshotter: self.snapshotter().to_string(),
            key: key.to_string(),
            parent: parent.to_string(),
            labels: Default::default(),
        };

        self.raw()
            .snapshots()
            .prepare(with_namespace!(request, self.namespace()))
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

        self.prepare_snapshot(&spec.id, &chain_id).await?;

        let result = self.create_container_record(spec, &image_config).await;
        if let Err(error) = result {
            // Roll the snapshot back rather than leaving an orphan for a
            // create that did not complete.
            let _ = self.remove_snapshot(&spec.id).await;
            return Err(error);
        }
        Ok(())
    }

    async fn create_container_record(
        &self,
        spec: &ContainerSpec,
        image_config: &ImageConfig,
    ) -> Result<()> {
        let oci = oci_spec(spec, image_config, self.namespace());
        let oci_bytes = serde_json::to_vec(&oci).context("failed to serialise the OCI spec")?;

        let container = Container {
            id: spec.id.clone(),
            image: spec.image.clone(),
            runtime: Some(Runtime {
                name: crate::config::RUNTIME.to_string(),
                options: None,
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
fn oci_spec(
    spec: &ContainerSpec,
    image_config: &ImageConfig,
    namespace: &str,
) -> serde_json::Value {
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
            "args": image_config.args(),
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
            // Non-systemd form, matching containerd's default. Task start
            // (Milestone 5) needs the systemd driver and a slice-qualified
            // path instead; Milestone 4 creates no task, so this is not
            // exercised yet. See the Milestone 5 note in SPEC.md.
            "cgroupsPath": format!("/{namespace}/{}", spec.id),
            "namespaces": [
                { "type": "pid" }, { "type": "ipc" }, { "type": "uts" },
                { "type": "mount" }, { "type": "network" }
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
    "CAP_CHOWN", "CAP_DAC_OVERRIDE", "CAP_FSETID", "CAP_FOWNER", "CAP_MKNOD",
    "CAP_NET_RAW", "CAP_SETGID", "CAP_SETUID", "CAP_SETFCAP", "CAP_SETPCAP",
    "CAP_NET_BIND_SERVICE", "CAP_SYS_CHROOT", "CAP_KILL", "CAP_AUDIT_WRITE",
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
