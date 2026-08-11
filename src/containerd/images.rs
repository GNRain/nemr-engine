//! Image operations.
//!
//! Milestone 1 surface: [`ContainerdClient::list_images`]. Image pull/import
//! arrives in Milestone 2 and extends this module rather than duplicating it.

use anyhow::{Context, Result};
use containerd_client::services::v1::ListImagesRequest;
use containerd_client::with_namespace;
use containerd_client::tonic::Request;

use super::client::ContainerdClient;

/// One image, reduced to the fields the engine actually uses.
///
/// A projection rather than a re-export of the generated protobuf `Image`:
/// the generated type carries timestamps, labels and a nested descriptor that
/// no Phase 1 caller needs, and leaking it would make every consumer depend on
/// the protobuf schema — exactly the coupling Section 3.2 exists to prevent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageSummary {
    /// Image reference, e.g. `docker.io/library/alpine:latest`.
    pub name: String,
    /// Digest of the image's target descriptor, e.g. `sha256:...`.
    pub digest: String,
    /// Size of the target descriptor in bytes.
    pub size: i64,
}

impl ContainerdClient {
    /// List all images in this client's namespace.
    ///
    /// Sorted by name so output is deterministic across calls — containerd
    /// does not guarantee ordering, and AC-1.2 compares this against an
    /// independently produced baseline.
    pub async fn list_images(&self) -> Result<Vec<ImageSummary>> {
        let request = ListImagesRequest { filters: vec![] };

        let response = self
            .raw()
            .images()
            .list(with_namespace!(request, self.namespace()))
            .await
            .context("containerd ListImages request failed")?;

        let mut images: Vec<ImageSummary> = response
            .into_inner()
            .images
            .into_iter()
            .map(|image| {
                // `target` is optional in the protobuf schema, though containerd
                // always populates it in practice. Degrade to empty values
                // rather than unwrapping on a server-controlled field.
                let (digest, size) = image
                    .target
                    .map(|t| (t.digest, t.size))
                    .unwrap_or_else(|| (String::new(), 0));

                ImageSummary {
                    name: image.name,
                    digest,
                    size,
                }
            })
            .collect();

        images.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(images)
    }
}
