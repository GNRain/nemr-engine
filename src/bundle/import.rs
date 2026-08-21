//! Reading a bundle (M10).
//!
//! The ordering is the point: **decide, verify, then extract.** A reader must be
//! able to refuse a bundle it cannot handle before it has written anything, and
//! must never write content it has not verified. Half-extracting and then
//! failing leaves a project in a state neither the user nor the engine can
//! reason about, which is worse than refusing.
//!
//! So the sequence is:
//!   1. Read the manifest — it is the archive's first member, so this costs one
//!      record, not a scan.
//!   2. Check schema compatibility, base image digest, and quota. All three can
//!      refuse before a byte of content is touched.
//!   3. Read chunks, verifying each plaintext digest as it is decompressed.
//!   4. Extract members, verifying each member digest against the manifest.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

use super::export::hex;
use super::manifest::{Manifest, MemberEntry, CHUNK_PREFIX, MANIFEST_MEMBER};

/// A bundle opened far enough to be reasoned about, but not yet extracted.
#[derive(Debug)]
pub struct Bundle {
    pub manifest: Manifest,
    /// Compressed chunk payloads, indexed by chunk number.
    chunks: BTreeMap<u32, Vec<u8>>,
}

/// What the destination must satisfy for an import to be allowed.
pub struct ImportChecks<'a> {
    /// Digest of the base image present on this host, if any. `None` means the
    /// image is absent entirely.
    pub local_base_image_digest: Option<&'a str>,
    /// Usable capacity of the destination volume, in bytes.
    pub destination_capacity: u64,
    /// The destination's quota preset, for the error message.
    pub destination_quota: &'a str,
}

/// Open a bundle: read the manifest and chunk payloads, verifying structure.
///
/// Does not extract, and does not touch the destination. A caller can open a
/// bundle purely to inspect it (`nemr inspect` later) without any side effect.
pub fn open(path: &Path) -> Result<Bundle> {
    let file = std::fs::File::open(path).map_err(|e| Error::BundleCorrupt {
        path: path.to_path_buf(),
        detail: format!("cannot open: {e}"),
    })?;
    let mut archive = tar::Archive::new(file);
    let mut entries = archive.entries().map_err(|e| Error::BundleCorrupt {
        path: path.to_path_buf(),
        detail: format!("not a readable archive: {e}"),
    })?;

    // The manifest must be first. A bundle that buries it somewhere else is
    // rejected rather than scanned for: the guarantee that compatibility can be
    // decided before reading content is only worth having if it is enforced.
    let mut first = entries
        .next()
        .transpose()
        .map_err(|e| Error::BundleCorrupt {
            path: path.to_path_buf(),
            detail: format!("unreadable first member: {e}"),
        })?
        .ok_or_else(|| Error::BundleCorrupt {
            path: path.to_path_buf(),
            detail: "the archive is empty".to_string(),
        })?;

    let first_name = first
        .path()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    if first_name != MANIFEST_MEMBER {
        return Err(Error::BundleCorrupt {
            path: path.to_path_buf(),
            detail: format!("first member is {first_name:?}, expected {MANIFEST_MEMBER:?}"),
        });
    }

    let mut manifest_bytes = Vec::new();
    first
        .read_to_end(&mut manifest_bytes)
        .map_err(|e| Error::BundleCorrupt {
            path: path.to_path_buf(),
            detail: format!("truncated manifest: {e}"),
        })?;
    let manifest: Manifest =
        serde_json::from_slice(&manifest_bytes).map_err(|e| Error::BundleCorrupt {
            path: path.to_path_buf(),
            detail: format!("manifest does not parse: {e}"),
        })?;

    // Refuse an unsupported schema before reading any content.
    manifest.compatibility()?;

    let mut chunks = BTreeMap::new();
    for entry in entries {
        let mut entry = entry.map_err(|e| Error::BundleCorrupt {
            path: path.to_path_buf(),
            detail: format!("unreadable member: {e}"),
        })?;
        let name = entry
            .path()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let Some(index) = chunk_index(&name) else {
            continue; // unknown members are ignored, per the compatibility rules
        };
        let mut payload = Vec::new();
        entry
            .read_to_end(&mut payload)
            .map_err(|e| Error::BundleCorrupt {
                path: path.to_path_buf(),
                detail: format!("truncated chunk {index}: {e}"),
            })?;
        chunks.insert(index, payload);
    }

    // Every chunk the manifest promises must be present. A missing chunk found
    // here is a clear error; found during extraction it would be a partially
    // written project.
    for chunk in &manifest.chunks {
        if !chunks.contains_key(&chunk.index) {
            return Err(Error::BundleCorrupt {
                path: path.to_path_buf(),
                detail: format!(
                    "manifest lists {} chunks but chunk {} is missing (truncated bundle?)",
                    manifest.chunks.len(),
                    chunk.index
                ),
            });
        }
    }

    Ok(Bundle { manifest, chunks })
}

fn chunk_index(name: &str) -> Option<u32> {
    name.strip_prefix(CHUNK_PREFIX)?
        .strip_suffix(".zst")?
        .parse()
        .ok()
}

impl Bundle {
    /// Refuse the import if the destination cannot satisfy the bundle.
    ///
    /// All checks happen before extraction, so a refusal leaves the destination
    /// untouched.
    pub fn check(&self, checks: &ImportChecks<'_>) -> Result<()> {
        // Base image: the digest is authoritative, the reference is a hint.
        // Substituting a different image would restore a session onto a rootfs
        // it was not created against — a silent wrong result rather than an
        // error, which is the failure class this project keeps hitting.
        match checks.local_base_image_digest {
            Some(local) if local == self.manifest.base_image.digest => {}
            _ => {
                return Err(Error::BaseImageMissing {
                    reference: self.manifest.base_image.reference.clone(),
                    digest: self.manifest.base_image.digest.clone(),
                })
            }
        }

        // Quota: checked up front so a too-small destination fails immediately
        // rather than half-way through extraction.
        if self.manifest.project.content_bytes > checks.destination_capacity {
            return Err(Error::QuotaMismatch {
                needed: crate::engine::volume::human_bytes(self.manifest.project.content_bytes),
                quota: checks.destination_quota.to_string(),
            });
        }
        Ok(())
    }

    /// Reassemble and verify the plaintext stream.
    ///
    /// Each chunk's digest is checked against the manifest as it is
    /// decompressed, so corruption is caught before any of it is written.
    fn plaintext(&self) -> Result<Vec<u8>> {
        let mut stream = Vec::with_capacity(self.manifest.project.content_bytes as usize);
        for entry in &self.manifest.chunks {
            let compressed = self
                .chunks
                .get(&entry.index)
                .expect("presence checked in open()");
            let plain = zstd::decode_all(compressed.as_slice()).map_err(|e| {
                Error::BundleCorrupt {
                    path: Path::new("<bundle>").to_path_buf(),
                    detail: format!("chunk {} does not decompress: {e}", entry.index),
                }
            })?;

            let actual = hex(&Sha256::digest(&plain));
            if actual != entry.sha256 {
                return Err(Error::ChecksumMismatch {
                    member: format!("chunk {}", entry.index),
                    expected: entry.sha256.clone(),
                    actual,
                });
            }
            stream.extend_from_slice(&plain);
        }
        Ok(stream)
    }

    /// Extract into `destination_root`, verifying every member digest.
    ///
    /// Session-critical members are written first, so a failure part-way leaves
    /// the session usable rather than the caches restored and the history
    /// missing.
    pub fn extract(&self, destination_root: &Path) -> Result<ExtractSummary> {
        let stream = self.plaintext()?;

        let mut ordered: Vec<&MemberEntry> = self.manifest.members.iter().collect();
        ordered.sort_by_key(|m| !m.is_session_critical());

        let mut written = 0usize;
        let mut bytes = 0u64;
        for member in ordered {
            let start = member.span.offset as usize;
            let end = start + member.span.length as usize;
            if end > stream.len() {
                return Err(Error::BundleCorrupt {
                    path: destination_root.to_path_buf(),
                    detail: format!(
                        "member {} claims bytes {start}..{end} but the stream is {} bytes",
                        member.path,
                        stream.len()
                    ),
                });
            }
            let content = &stream[start..end];

            let actual = hex(&Sha256::digest(content));
            if actual != member.sha256 {
                return Err(Error::ChecksumMismatch {
                    member: member.path.clone(),
                    expected: member.sha256.clone(),
                    actual,
                });
            }

            let target = safe_join(destination_root, &member.path)?;
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    Error::Internal(
                        anyhow::Error::from(e).context(format!("creating {}", parent.display())),
                    )
                })?;
            }
            std::fs::write(&target, content).map_err(|e| {
                Error::Internal(
                    anyhow::Error::from(e).context(format!("writing {}", target.display())),
                )
            })?;
            written += 1;
            bytes += member.span.length;
        }

        Ok(ExtractSummary {
            members: written,
            bytes,
        })
    }
}

#[derive(Debug)]
pub struct ExtractSummary {
    pub members: usize,
    pub bytes: u64,
}

/// Join a manifest-supplied relative path onto a root, refusing traversal.
///
/// A bundle is untrusted input — it may have arrived over a network or from
/// another user — so a member path of `../../etc/passwd` must be refused rather
/// than written. This is the same class of defect as the privileged helper's
/// mount-point escape, in a different place.
fn safe_join(root: &Path, relative: &str) -> Result<std::path::PathBuf> {
    use std::path::Component;

    let candidate = Path::new(relative);
    for component in candidate.components() {
        match component {
            Component::Normal(_) => {}
            other => {
                return Err(Error::BundleCorrupt {
                    path: root.to_path_buf(),
                    detail: format!(
                        "member path {relative:?} contains a non-literal component {other:?}; \
                         refusing to write outside the destination"
                    ),
                })
            }
        }
    }
    Ok(root.join(candidate))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bundle::export::{export, ExportRequest};
    use crate::bundle::manifest::BaseImageRef;
    use crate::bundle::policy::Policy;
    use std::path::PathBuf;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nemr-import-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const DIGEST: &str = "sha256:deadbeef";

    fn make_bundle(tag: &str, files: &[(&str, &str)]) -> (PathBuf, PathBuf) {
        let root = scratch(tag);
        for (path, contents) in files {
            let full = root.join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, contents).unwrap();
        }
        let out = scratch(&format!("{tag}-out")).join("b.nemr");
        export(
            &ExportRequest {
                project: "demo",
                quota: "2GB",
                source_root: &root,
                base_image: BaseImageRef {
                    reference: "docker.io/nemr/base:0.1.0".into(),
                    digest: DIGEST.into(),
                },
                policy: Policy::default(),
            },
            &out,
        )
        .unwrap();
        (root, out)
    }

    fn checks<'a>() -> ImportChecks<'a> {
        ImportChecks {
            local_base_image_digest: Some(DIGEST),
            destination_capacity: 1 << 30,
            destination_quota: "2GB",
        }
    }

    /// The core round trip: what goes in comes out byte-identical.
    #[test]
    fn round_trip_restores_content_exactly() {
        let (_, bundle_path) = make_bundle(
            "roundtrip",
            &[
                (".nemr-state/projects/-workspace/a.jsonl", "conversation history"),
                ("notes.md", "project file"),
            ],
        );
        let bundle = open(&bundle_path).unwrap();
        bundle.check(&checks()).unwrap();

        let destination = scratch("roundtrip-dest");
        let summary = bundle.extract(&destination).unwrap();
        assert_eq!(summary.members, 2);

        assert_eq!(
            std::fs::read_to_string(destination.join(".nemr-state/projects/-workspace/a.jsonl"))
                .unwrap(),
            "conversation history",
            "the transcript must survive the round trip byte-identical"
        );
        assert_eq!(
            std::fs::read_to_string(destination.join("notes.md")).unwrap(),
            "project file"
        );
    }

    #[test]
    fn a_truncated_bundle_is_refused_with_a_clear_error() {
        let (_, bundle_path) = make_bundle("truncated", &[("a.txt", &"x".repeat(5000))]);
        let raw = std::fs::read(&bundle_path).unwrap();
        let truncated = bundle_path.with_extension("trunc");
        std::fs::write(&truncated, &raw[..raw.len() / 2]).unwrap();

        let error = open(&truncated).expect_err("a truncated bundle must be refused");
        assert_eq!(error.kind(), crate::error::ErrorKind::DataIntegrity);
    }

    #[test]
    fn a_bundle_that_is_not_an_archive_is_refused() {
        let path = scratch("garbage").join("b.nemr");
        std::fs::write(&path, b"this is not a tar archive at all").unwrap();
        let error = open(&path).expect_err("garbage must be refused");
        assert_eq!(error.kind(), crate::error::ErrorKind::DataIntegrity);
    }

    /// A corrupted chunk must be caught by its digest, before anything is
    /// written to the destination.
    #[test]
    fn a_corrupted_chunk_is_caught_before_extraction() {
        let (_, bundle_path) = make_bundle("corrupt-chunk", &[("a.txt", "original content")]);
        let mut bundle = open(&bundle_path).unwrap();

        // Replace the chunk with validly-compressed but different bytes.
        let tampered = zstd::encode_all(&b"tampered content"[..], 3).unwrap();
        bundle.chunks.insert(0, tampered);

        let destination = scratch("corrupt-chunk-dest");
        let error = bundle
            .extract(&destination)
            .expect_err("a chunk digest mismatch must be caught");
        assert_eq!(error.kind(), crate::error::ErrorKind::DataIntegrity);
        assert!(
            !destination.join("a.txt").exists(),
            "nothing may be written when verification fails"
        );
    }

    #[test]
    fn a_missing_base_image_refuses_rather_than_substituting() {
        let (_, bundle_path) = make_bundle("base-image", &[("a.txt", "x")]);
        let bundle = open(&bundle_path).unwrap();

        for local in [None, Some("sha256:a-different-image")] {
            let error = bundle
                .check(&ImportChecks {
                    local_base_image_digest: local,
                    ..checks()
                })
                .expect_err("a mismatched base image must refuse");
            assert_eq!(error.kind(), crate::error::ErrorKind::HostPrerequisite);
            assert!(
                error.to_string().contains("docker.io/nemr/base"),
                "the error names the image the bundle needs"
            );
        }
    }

    #[test]
    fn a_too_small_destination_refuses_before_extracting() {
        let (_, bundle_path) = make_bundle("quota", &[("a.txt", &"x".repeat(10_000))]);
        let bundle = open(&bundle_path).unwrap();
        let error = bundle
            .check(&ImportChecks {
                destination_capacity: 100,
                destination_quota: "500MB",
                ..checks()
            })
            .expect_err("a too-small destination must refuse");
        assert_eq!(error.kind(), crate::error::ErrorKind::CapacityExceeded);
    }

    /// A bundle is untrusted input. A member path that escapes the destination
    /// must be refused — the same defect class as the helper's mount-point
    /// escape, in a different place.
    #[test]
    fn member_paths_cannot_escape_the_destination() {
        let root = scratch("traversal");
        for evil in ["../escaped.txt", "/etc/passwd", "a/../../escaped.txt"] {
            assert!(
                safe_join(&root, evil).is_err(),
                "{evil:?} must be refused, not written"
            );
        }
        assert!(safe_join(&root, "nested/ok.txt").is_ok());
    }

    /// Session-critical members are written first, so a failure part-way leaves
    /// the session usable rather than the caches restored and history missing.
    #[test]
    fn session_critical_members_are_written_first() {
        let (_, bundle_path) = make_bundle(
            "ordering",
            &[
                (".nemr-state/backups/old.json", "reconstructible"),
                (".nemr-state/projects/-workspace/a.jsonl", "critical"),
            ],
        );
        let bundle = open(&bundle_path).unwrap();
        let mut ordered: Vec<&MemberEntry> = bundle.manifest.members.iter().collect();
        ordered.sort_by_key(|m| !m.is_session_critical());
        assert!(
            ordered[0].is_session_critical(),
            "the session-critical member must be restored first"
        );
    }
}
