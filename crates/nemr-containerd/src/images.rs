//! Image operations.
//!
//! Milestone 1 surface: [`ContainerdClient::list_images`]. Image pull/import
//! arrives in Milestone 2 and extends this module rather than duplicating it.

use anyhow::{bail, Context, Result};
use containerd_client::services::v1::{
    CreateImageRequest, DeleteImageRequest, GetImageRequest, Image, ListImagesRequest,
    ReadContentRequest,
};
use containerd_client::tonic::Request;
use containerd_client::with_namespace;
use sha2::{Digest, Sha256};

use super::client::ContainerdClient;

/// One image, reduced to the fields a consumer actually uses.
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

impl ContainerdClient {
    /// Record an additional reference for an image that is already present.
    ///
    /// Only the name changes: the target descriptor is reused, so no blobs move
    /// and both references resolve to the same digest. Exists so the D-08
    /// by-digest resolution can be tested against real containerd — proving that
    /// an image filed under a different name is still found requires a second
    /// name to file it under.
    pub async fn tag_image(&self, existing: &str, new_name: &str) -> Result<()> {
        let request = GetImageRequest {
            name: existing.to_string(),
        };
        let image = self
            .raw()
            .images()
            .get(with_namespace!(request, self.namespace()))
            .await
            .with_context(|| format!("image {existing:?} not found"))?
            .into_inner()
            .image
            .with_context(|| format!("image {existing:?} has no record"))?;

        let request = CreateImageRequest {
            image: Some(Image {
                name: new_name.to_string(),
                target: image.target,
                labels: image.labels,
                ..Default::default()
            }),
            source_date_epoch: None,
        };
        self.raw()
            .images()
            .create(with_namespace!(request, self.namespace()))
            .await
            .with_context(|| format!("failed to record image reference {new_name:?}"))?;
        Ok(())
    }

    /// Remove an image *reference*. Blobs referenced elsewhere are untouched.
    pub async fn untag_image(&self, name: &str) -> Result<()> {
        let request = DeleteImageRequest {
            name: name.to_string(),
            sync: true,
            target: None,
        };
        self.raw()
            .images()
            .delete(with_namespace!(request, self.namespace()))
            .await
            .with_context(|| format!("failed to remove image reference {name:?}"))?;
        Ok(())
    }

    /// Find a locally-stored image whose target digest is `digest`.
    ///
    /// D-08 part 2: the digest is what identifies a base image, not the name it
    /// happens to be filed under. An image pulled by digest, imported under a
    /// different tag, or carried in a bundle is the *same image* if the digest
    /// matches — so resolution must ask "are these bytes here?", not "is
    /// something called `X` here?".
    ///
    /// Looking up by name only, as the import path did, meant a host holding
    /// exactly the right image under any other reference would be told to go to
    /// the registry. That is a needless network dependency on a machine that
    /// already has the bytes, and D-08's whole point is that the product must
    /// not need the registry when it does not have to.
    ///
    /// Returns every match, because more than one reference can point at the
    /// same digest and reporting only the first would hide that.
    pub async fn images_with_digest(&self, digest: &str) -> Result<Vec<ImageSummary>> {
        Ok(self
            .list_images()
            .await?
            .into_iter()
            .filter(|image| image.digest == digest)
            .collect())
    }

    /// Fetch an image record and return its target descriptor digest.
    ///
    /// Fails with a message naming the image if it is absent, since "image not
    /// found" is the most likely first-run error and the fix (build and import
    /// the base image, Milestone 2) is not guessable from a gRPC NotFound.
    pub async fn image_target_digest(&self, name: &str) -> Result<String> {
        let request = GetImageRequest {
            name: name.to_string(),
        };

        let response = self
            .raw()
            .images()
            .get(with_namespace!(request, self.namespace()))
            .await
            .with_context(|| {
                format!(
                    "image {name:?} not found in containerd namespace {:?}. \
                     Build and import the base image first (see README, Milestone 2).",
                    self.namespace()
                )
            })?;

        response
            .into_inner()
            .image
            .and_then(|image| image.target)
            .map(|target| target.digest)
            .with_context(|| format!("image {name:?} has no target descriptor"))
    }

    /// Read a blob out of the content store.
    ///
    /// Reads happen server-side: containerd streams the bytes back over gRPC.
    /// Nothing is mounted, which is why image inspection works from outside
    /// rootlesskit's namespaces.
    pub async fn read_blob(&self, digest: &str) -> Result<Vec<u8>> {
        let request = ReadContentRequest {
            digest: digest.to_string(),
            offset: 0,
            size: 0,
        };

        let mut stream = self
            .raw()
            .content()
            .read(with_namespace!(request, self.namespace()))
            .await
            .with_context(|| format!("failed to read blob {digest}"))?
            .into_inner();

        let mut bytes = Vec::new();
        while let Some(chunk) = stream
            .message()
            .await
            .with_context(|| format!("error streaming blob {digest}"))?
        {
            bytes.extend_from_slice(&chunk.data);
        }

        if bytes.is_empty() {
            bail!("blob {digest} was empty");
        }
        Ok(bytes)
    }

    /// Compute the rootfs chain ID for an image.
    ///
    /// This is the identifier a snapshotter uses for the fully-applied layer
    /// stack, and it is what a new snapshot takes as its parent. Deriving it
    /// means walking image -> manifest -> config -> `rootfs.diff_ids`, then
    /// folding the diff IDs together.
    ///
    /// Only single-platform OCI manifests are handled. The base image built at
    /// Milestone 2 is one; a multi-platform index would need a platform match
    /// first, which no Phase 1 requirement calls for.
    pub async fn image_chain_id(&self, name: &str) -> Result<String> {
        let manifest_digest = self.image_target_digest(name).await?;
        let manifest_bytes = self.read_blob(&manifest_digest).await?;

        let manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes)
            .with_context(|| format!("image {name:?} manifest is not valid JSON"))?;

        // Reject an index rather than silently misreading it as a manifest.
        if manifest.get("manifests").is_some() {
            bail!(
                "image {name:?} is a multi-platform index. Phase 1 handles single-platform \
                 manifests only; import a platform-specific image."
            );
        }

        let config_digest = manifest
            .pointer("/config/digest")
            .and_then(|v| v.as_str())
            .with_context(|| format!("image {name:?} manifest has no config digest"))?;

        let config_bytes = self.read_blob(config_digest).await?;
        let config: serde_json::Value = serde_json::from_slice(&config_bytes)
            .with_context(|| format!("image {name:?} config is not valid JSON"))?;

        let diff_ids: Vec<String> = config
            .pointer("/rootfs/diff_ids")
            .and_then(|v| v.as_array())
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| id.as_str().map(str::to_string))
                    .collect()
            })
            .with_context(|| format!("image {name:?} config has no rootfs.diff_ids"))?;

        chain_id(&diff_ids).with_context(|| format!("cannot compute chain ID for image {name:?}"))
    }
}

/// Fold a layer's diff IDs into an OCI chain ID.
///
/// Defined by the image spec as:
///
/// ```text
/// ChainID(L0)       = DiffID(L0)
/// ChainID(L0…Ln)    = SHA256( ChainID(L0…Ln-1) + " " + DiffID(Ln) )
/// ```
///
/// The digests are hashed as their full string form, prefix included — a
/// detail that is easy to get wrong and produces a plausible-looking but
/// unusable ID, since the snapshot parent simply will not exist.
fn chain_id(diff_ids: &[String]) -> Result<String> {
    let mut iter = diff_ids.iter();
    let mut chain = iter.next().context("image has no layers")?.clone();

    for diff_id in iter {
        let mut hasher = Sha256::new();
        hasher.update(format!("{chain} {diff_id}").as_bytes());
        chain = format!("sha256:{}", hex(&hasher.finalize()));
    }
    Ok(chain)
}

/// Lowercase hex encoding.
///
/// Written out rather than using `{:x}` on the digest: `sha2` 0.11 returns a
/// `hybrid_array::Array`, which does not implement `LowerHex`. Doing it here
/// keeps the code working across that API change either way.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

#[cfg(test)]
mod chain_id_tests {
    use super::chain_id;

    /// A single-layer image's chain ID is its only diff ID, unhashed.
    #[test]
    fn single_layer_is_its_own_chain_id() {
        let ids = vec!["sha256:aaaa".to_string()];
        assert_eq!(chain_id(&ids).unwrap(), "sha256:aaaa");
    }

    /// Known-answer test against the OCI definition, so a refactor cannot
    /// silently change the folding rule.
    #[test]
    fn two_layers_fold_per_the_oci_definition() {
        use sha2::{Digest, Sha256};
        let a = "sha256:aaaa".to_string();
        let b = "sha256:bbbb".to_string();

        let mut hasher = Sha256::new();
        hasher.update(format!("{a} {b}").as_bytes());
        let expected = format!("sha256:{}", super::hex(&hasher.finalize()));

        assert_eq!(chain_id(&[a, b]).unwrap(), expected);
    }

    #[test]
    fn empty_layer_list_is_an_error() {
        assert!(chain_id(&[]).is_err());
    }
}

/// The parts of an image's config needed to build a runtime spec.
#[derive(Debug, Clone, Default)]
pub struct ImageConfig {
    pub env: Vec<String>,
    pub entrypoint: Vec<String>,
    pub cmd: Vec<String>,
    pub working_dir: Option<String>,
}

impl ImageConfig {
    /// Process arguments, per the OCI rule that entrypoint precedes cmd.
    pub fn args(&self) -> Vec<String> {
        let mut args = self.entrypoint.clone();
        args.extend(self.cmd.iter().cloned());
        args
    }
}

impl ContainerdClient {
    /// Read an image's config (env, entrypoint, cmd, working dir).
    ///
    /// The engine builds its runtime spec from this rather than hardcoding the
    /// base image's values, so a change to `image/Dockerfile` does not silently
    /// desynchronise from the caller.
    pub async fn image_config(&self, name: &str) -> Result<ImageConfig> {
        let manifest_digest = self.image_target_digest(name).await?;
        let manifest: serde_json::Value =
            serde_json::from_slice(&self.read_blob(&manifest_digest).await?)
                .with_context(|| format!("image {name:?} manifest is not valid JSON"))?;

        let config_digest = manifest
            .pointer("/config/digest")
            .and_then(|v| v.as_str())
            .with_context(|| format!("image {name:?} manifest has no config digest"))?;

        let config: serde_json::Value =
            serde_json::from_slice(&self.read_blob(config_digest).await?)
                .with_context(|| format!("image {name:?} config is not valid JSON"))?;

        let strings = |pointer: &str| -> Vec<String> {
            config
                .pointer(pointer)
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|s| s.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default()
        };

        Ok(ImageConfig {
            env: strings("/config/Env"),
            entrypoint: strings("/config/Entrypoint"),
            cmd: strings("/config/Cmd"),
            working_dir: config
                .pointer("/config/WorkingDir")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string),
        })
    }
}
