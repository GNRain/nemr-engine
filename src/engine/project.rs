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
use crate::engine::volume::{HelperOps, Volume, VolumePaths, VolumeSize};

/// Label keys written onto the container record.
///
/// Prefixed so engine metadata is distinguishable from anything else that
/// might label a container in this namespace.
pub const LABEL_PROJECT: &str = "aihub.project";
pub const LABEL_VOLUME: &str = "aihub.volume";
pub const LABEL_SIZE: &str = "aihub.size";

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
            assert!(key.starts_with("aihub."), "{key} should be namespaced");
        }
    }

    #[test]
    fn container_id_is_prefixed() {
        assert_eq!(config::container_id("demo"), "aihub-demo");
    }
}
