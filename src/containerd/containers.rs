//! Container operations.
//!
//! Milestone 1 surface: [`ContainerdClient::list_containers`]. Create, start,
//! stop and delete arrive in Milestones 4–5 and extend this module.

use anyhow::{Context, Result};
use containerd_client::services::v1::ListContainersRequest;
use containerd_client::with_namespace;
use containerd_client::tonic::Request;

use super::client::ContainerdClient;

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
