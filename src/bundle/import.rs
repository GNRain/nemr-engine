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
/// The outcome of looking for the base image a bundle needs.
///
/// D-08: the digest identifies the image, not the name it happens to be filed
/// under. An image pulled by digest, imported under a different tag, or carried
/// in a bundle is the *same image* if the digest matches, so resolution asks
/// "are these bytes here?" rather than "is something called X here?".
///
/// Carrying the failed attempts rather than a bare `None` is deliberate: the
/// user needs to know whether the registry was unreachable or answered and did
/// not have it, because those have different fixes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaseImageResolution {
    /// Found, digest verified. `reference` is where it was found, which may
    /// differ from the reference the bundle names.
    Present { reference: String },
    /// Not found. `where_looked` lists each attempt in order.
    Unresolved {
        where_looked: Vec<String>,
        advice: String,
    },
}

pub struct ImportChecks<'a> {
    /// How the bundle's base image was resolved, and if it was not, where the
    /// caller looked. Built by [`resolve_base_image`].
    pub base_image: BaseImageResolution,
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
    /// The digest of the base image this bundle was created from.
    pub fn base_image_digest(&self) -> &str {
        &self.manifest.base_image.digest
    }

    /// The reference (repo:version) the base image was recorded under.
    ///
    /// Used by resolution to advise pulling *the version this bundle needs*
    /// (F-85), not whatever version the engine currently defaults to — pulling
    /// the current version would fetch different bytes and fail confusingly.
    pub fn base_image_reference(&self) -> &str {
        &self.manifest.base_image.reference
    }

    /// The rootfs chain id the bundle records, or `""` for a bundle written
    /// before the field existed (F-115).
    pub fn base_image_chain_id(&self) -> &str {
        &self.manifest.base_image.rootfs_chain_id
    }

    pub fn check(&self, checks: &ImportChecks<'_>) -> Result<()> {
        // Base image: the digest is authoritative, the reference is a hint.
        // Substituting a different image would restore a session onto a rootfs
        // it was not created against — a silent wrong result rather than an
        // error, which is the failure class this project keeps hitting.
        match checks.base_image {
            BaseImageResolution::Present { .. } => {}
            BaseImageResolution::Unresolved {
                ref where_looked,
                ref advice,
            } => {
                return Err(Error::BaseImageUnresolved {
                    reference: self.manifest.base_image.reference.clone(),
                    digest: self.manifest.base_image.digest.clone(),
                    where_looked: where_looked
                        .iter()
                        .map(|line| format!("  - {line}"))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    advice: advice.clone(),
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
            let plain =
                zstd::decode_all(compressed.as_slice()).map_err(|e| Error::BundleCorrupt {
                    path: Path::new("<bundle>").to_path_buf(),
                    detail: format!("chunk {} does not decompress: {e}", entry.index),
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

    /// Write an arbitrary bundle: any manifest, any chunk payloads.
    ///
    /// M11 hardening cases are about bundles a *hostile or broken* producer
    /// creates, which `export()` cannot make by construction. Building them as
    /// real archive files means the cases go through the real
    /// `open()` -> `check()` -> `extract()` path rather than a hand-mutated
    /// struct, so a guard that exists but is never *called* still fails the test
    /// (the F-58 lesson).
    fn write_hostile_bundle(
        tag: &str,
        manifest: &Manifest,
        chunks: &[Vec<u8>],
        manifest_first: bool,
    ) -> PathBuf {
        let path = scratch(tag).join("hostile.nemr");
        let file = std::fs::File::create(&path).unwrap();
        let mut builder = tar::Builder::new(file);

        let append = |builder: &mut tar::Builder<std::fs::File>, name: &str, bytes: &[u8]| {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_mtime(0);
            header.set_cksum();
            builder.append_data(&mut header, name, bytes).unwrap();
        };

        let json = serde_json::to_vec(manifest).unwrap();
        if !manifest_first {
            append(&mut builder, "chunks/0000.zst", b"decoy");
        }
        append(&mut builder, MANIFEST_MEMBER, &json);
        for (index, chunk) in chunks.iter().enumerate() {
            append(&mut builder, &format!("chunks/{index:04}.zst"), chunk);
        }
        builder.finish().unwrap();
        path
    }

    /// A minimal well-formed manifest, for hardening cases to bend.
    fn base_manifest(
        members: Vec<MemberEntry>,
        chunks: Vec<crate::bundle::manifest::ChunkEntry>,
        content_bytes: u64,
    ) -> Manifest {
        Manifest {
            schema_version: crate::bundle::SCHEMA_VERSION,
            engine_version: "0.1.0".into(),
            created_at: "0".into(),
            project: crate::bundle::manifest::ProjectInfo {
                name: "demo".into(),
                quota: "2GB".into(),
                content_bytes,
                agent: "claude-code".into(),
            },
            base_image: BaseImageRef {
                reference: "ghcr.io/gnrain/nemr-base:0.2.0".into(),
                digest: DIGEST.into(),
                rootfs_chain_id: String::new(),
            },
            chunks,
            members,
            excluded: vec![],
        }
    }

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
                agent: "claude-code",
                source_root: &root,
                base_image: BaseImageRef {
                    reference: "ghcr.io/gnrain/nemr-base:0.2.0".into(),
                    digest: DIGEST.into(),
                    rootfs_chain_id: String::new(),
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
            base_image: BaseImageResolution::Present {
                reference: "ghcr.io/gnrain/nemr-base:0.2.0".into(),
            },
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
                (
                    ".nemr-state/projects/-workspace/a.jsonl",
                    "conversation history",
                ),
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

    /// A future-schema bundle must be refused by `open()`, not merely by the
    /// helper the manifest test calls directly.
    ///
    /// `a_newer_schema_is_refused_with_advice` exercises `Manifest::compatibility`
    /// on a hand-built struct; deleting the `manifest.compatibility()?` call from
    /// `open()` left it green while a future-version bundle would be extracted
    /// under assumptions this build cannot know (F-56 class, guard-test audit).
    /// This one rewrites a real bundle's manifest and goes through `open()`.
    #[test]
    fn a_future_schema_bundle_is_refused_by_open() {
        let (_, bundle_path) = make_bundle("future-schema", &[("a.txt", "x")]);

        // Rebuild the archive with the schema version bumped beyond this build.
        let mut manifest: crate::bundle::manifest::Manifest = {
            let file = std::fs::File::open(&bundle_path).unwrap();
            let mut archive = tar::Archive::new(file);
            let mut entry = archive.entries().unwrap().next().unwrap().unwrap();
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut bytes).unwrap();
            serde_json::from_slice(&bytes).unwrap()
        };
        manifest.schema_version = crate::bundle::SCHEMA_VERSION + 1;

        let future = bundle_path.with_extension("future");
        {
            let out = std::fs::File::create(&future).unwrap();
            let mut builder = tar::Builder::new(out);
            let json = serde_json::to_vec(&manifest).unwrap();
            let mut header = tar::Header::new_gnu();
            header.set_size(json.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, MANIFEST_MEMBER, json.as_slice())
                .unwrap();
            builder.finish().unwrap();
        }

        let error = open(&future).expect_err("a future-schema bundle must be refused");
        assert_eq!(error.kind(), crate::error::ErrorKind::Incompatible);
        assert!(
            error.to_string().contains("Upgrade nemr"),
            "the refusal must say what to do: {error}"
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
        // Assert WHICH layer caught it. Without this, deleting the chunk-digest
        // check in plaintext() left the test green — the tampered bytes flowed on
        // and the per-member check rejected them with the same kind, so the
        // chunk-level guard could vanish unnoticed (F-58).
        assert!(
            error.to_string().contains("chunk 0"),
            "corruption must be caught at the CHUNK layer: {error}"
        );
        assert!(
            !destination.join("a.txt").exists(),
            "nothing may be written when verification fails"
        );
    }

    #[test]
    fn a_missing_base_image_refuses_rather_than_substituting() {
        let (_, bundle_path) = make_bundle("base-image", &[("a.txt", "x")]);
        let bundle = open(&bundle_path).unwrap();

        for label in ["absent", "digest mismatch"] {
            let error = bundle
                .check(&ImportChecks {
                    base_image: BaseImageResolution::Unresolved {
                        where_looked: vec![format!("local containerd: {label}")],
                        advice: "Build and import the base image.".into(),
                    },
                    ..checks()
                })
                .expect_err("a mismatched base image must refuse");
            assert_eq!(error.kind(), crate::error::ErrorKind::HostPrerequisite);
            assert!(
                error.to_string().contains("ghcr.io/gnrain/nemr-base"),
                "the error names the image the bundle needs"
            );
        }
    }

    /// D-08: the refusal must say **where** resolution was tried.
    ///
    /// "not present on this host" was true and useless — it did not say whether
    /// a registry had been consulted, so a user could not tell a missing image
    /// from a broken network. The distinction is the whole requirement.
    #[test]
    fn an_unresolved_base_image_says_where_it_looked() {
        let (_, bundle_path) = make_bundle("where-looked", &[("a.txt", "x")]);
        let bundle = open(&bundle_path).unwrap();

        let error = bundle
            .check(&ImportChecks {
                base_image: BaseImageResolution::Unresolved {
                    where_looked: vec![
                        "local containerd, by name ghcr.io/gnrain/nemr-base:0.2.0: not present"
                            .into(),
                        "local containerd, by digest across all images: no match".into(),
                        "no registry was contacted: this build resolves locally only".into(),
                    ],
                    advice: "Build and import the base image.".into(),
                },
                ..checks()
            })
            .expect_err("an unresolved base image must refuse");

        let text = error.to_string();
        assert!(text.contains(DIGEST), "must name the digest: {text}");
        for attempt in [
            "by name",
            "by digest across all images",
            "no registry was contacted",
        ] {
            assert!(
                text.contains(attempt),
                "the error must report the {attempt:?} attempt: {text}"
            );
        }
        assert!(
            text.contains("Build and import"),
            "must say what to do next: {text}"
        );
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

    /// `extract()` must *use* the traversal guard, not merely have one.
    ///
    /// The unit test below exercises `safe_join` directly; replacing the call
    /// site with a plain `destination_root.join(&member.path)` left every test
    /// green while a hostile bundle wrote outside the destination (F-58). A
    /// bundle is untrusted input, so this drives a malicious member path through
    /// the real extract path. Proven to fail when the call site is bypassed.
    #[test]
    fn extract_refuses_a_member_path_that_escapes_the_destination() {
        let (_, bundle_path) = make_bundle("escape", &[("a.txt", "content")]);
        let mut bundle = open(&bundle_path).unwrap();

        let plain = b"PWNED".to_vec();
        bundle.chunks.clear();
        bundle
            .chunks
            .insert(0, zstd::encode_all(plain.as_slice(), 3).unwrap());
        bundle.manifest.chunks = vec![crate::bundle::manifest::ChunkEntry {
            index: 0,
            sha256: hex(&Sha256::digest(&plain)),
            compressed_bytes: 0,
            plain_bytes: plain.len() as u64,
        }];
        bundle.manifest.members = vec![MemberEntry {
            path: "../escaped.txt".into(),
            class: crate::bundle::policy::Class::SessionCritical
                .as_str()
                .into(),
            mode: 0o100644,
            size: plain.len() as u64,
            sha256: hex(&Sha256::digest(&plain)),
            span: crate::bundle::manifest::Span {
                offset: 0,
                length: plain.len() as u64,
            },
        }];

        let destination = scratch("escape-dest").join("inner");
        std::fs::create_dir_all(&destination).unwrap();
        let error = bundle
            .extract(&destination)
            .expect_err("a member path escaping the destination must be refused");
        assert_eq!(error.kind(), crate::error::ErrorKind::DataIntegrity);

        let escaped = destination.parent().unwrap().join("escaped.txt");
        assert!(
            !escaped.exists(),
            "nothing may be written outside the destination: {} exists",
            escaped.display()
        );
    }

    // ---------------------------------------------------------------------
    // M11 — hardening. Each case is a real archive a hostile or broken producer
    // could write, driven through open() -> check() -> extract().
    // ---------------------------------------------------------------------

    /// M11: a bundle whose manifest is not the first member is refused.
    ///
    /// The whole "decide before reading content" guarantee rests on the manifest
    /// being first; a reader that scanned for it would silently accept an
    /// archive that buries it behind arbitrary content.
    #[test]
    fn m11_a_bundle_with_a_buried_manifest_is_refused() {
        let manifest = base_manifest(vec![], vec![], 0);
        let path = write_hostile_bundle("buried", &manifest, &[], false);

        let error = open(&path).expect_err("a buried manifest must be refused");
        assert_eq!(error.kind(), crate::error::ErrorKind::DataIntegrity);
        // Assert on the SPECIFIC refusal, naming the member actually found.
        //
        // A first attempt asserted `contains("expected")`, which passed with the
        // check disabled: serde's parse error for the decoy member is "expected
        // value at line 1 column 1", so the assertion matched by coincidence.
        // Proven vacuous by the disable-and-watch-it-fail probe — the rule
        // catching a mistake made while applying the rule.
        let text = error.to_string();
        assert!(
            text.contains("chunks/0000.zst") && text.contains(MANIFEST_MEMBER),
            "the error must name what was found and what was expected: {text}"
        );
    }

    /// M11: a manifest promising more chunks than the archive carries is
    /// refused at open, before anything is written.
    #[test]
    fn m11_a_bundle_missing_a_promised_chunk_is_refused() {
        let plain = b"content".to_vec();
        let manifest = base_manifest(
            vec![],
            vec![
                crate::bundle::manifest::ChunkEntry {
                    index: 0,
                    sha256: hex(&Sha256::digest(&plain)),
                    compressed_bytes: 0,
                    plain_bytes: plain.len() as u64,
                },
                // Promised but never written.
                crate::bundle::manifest::ChunkEntry {
                    index: 1,
                    sha256: hex(&Sha256::digest(b"missing")),
                    compressed_bytes: 0,
                    plain_bytes: 7,
                },
            ],
            plain.len() as u64,
        );
        let path = write_hostile_bundle(
            "missing-chunk",
            &manifest,
            &[zstd::encode_all(plain.as_slice(), 3).unwrap()],
            true,
        );

        let error = open(&path).expect_err("a missing chunk must be refused");
        assert_eq!(error.kind(), crate::error::ErrorKind::DataIntegrity);
        assert!(
            error.to_string().contains("chunk 1"),
            "the error must name the missing chunk: {error}"
        );
    }

    /// M11: base image drift between source and destination refuses rather than
    /// substituting a different rootfs.
    ///
    /// Driven through `check()` with three destination states: absent, a
    /// different digest, and the matching digest as a control — so a pass cannot
    /// come from `check()` refusing everything.
    #[test]
    fn m11_base_image_drift_refuses_and_the_matching_digest_is_accepted() {
        let (_, bundle_path) = make_bundle("drift", &[("a.txt", "x")]);
        let bundle = open(&bundle_path).unwrap();

        for label in ["absent", "drifted"] {
            let error = bundle
                .check(&ImportChecks {
                    base_image: BaseImageResolution::Unresolved {
                        where_looked: vec![format!("local containerd, by name: {label}")],
                        advice: "Build and import the base image.".into(),
                    },
                    ..checks()
                })
                .unwrap_err();
            assert_eq!(
                error.kind(),
                crate::error::ErrorKind::HostPrerequisite,
                "{label} base image must refuse"
            );
            assert!(
                error.to_string().contains(DIGEST),
                "{label}: the error must name the digest the bundle needs: {error}"
            );
        }

        // CONTROL: the matching digest must be accepted, or the two refusals
        // above prove only that check() always fails.
        bundle
            .check(&checks())
            .expect("control: a matching base image must be accepted");
    }

    /// M11: quota mismatch refuses before extraction, and a sufficient
    /// destination is accepted.
    #[test]
    fn m11_quota_mismatch_refuses_before_extraction() {
        let (_, bundle_path) = make_bundle("quota-m11", &[("a.txt", &"x".repeat(10_000))]);
        let bundle = open(&bundle_path).unwrap();
        let needed = bundle.manifest.project.content_bytes;

        let error = bundle
            .check(&ImportChecks {
                destination_capacity: needed - 1,
                destination_quota: "500MB",
                ..checks()
            })
            .expect_err("a destination one byte too small must refuse");
        assert_eq!(error.kind(), crate::error::ErrorKind::CapacityExceeded);

        // CONTROL: exactly enough capacity must be accepted.
        bundle
            .check(&ImportChecks {
                destination_capacity: needed,
                ..checks()
            })
            .expect("control: exactly-sufficient capacity must be accepted");
    }

    /// M11: a member digest mismatch is caught, names the member, and leaves
    /// nothing of that member on disk.
    #[test]
    fn m11_member_digest_mismatch_names_the_member_and_writes_nothing() {
        let plain = b"the real content".to_vec();
        let manifest = base_manifest(
            vec![MemberEntry {
                path: "session/history.jsonl".into(),
                class: crate::bundle::policy::Class::SessionCritical
                    .as_str()
                    .into(),
                mode: 0o100644,
                size: plain.len() as u64,
                sha256: "0".repeat(64), // wrong on purpose
                span: crate::bundle::manifest::Span {
                    offset: 0,
                    length: plain.len() as u64,
                },
            }],
            vec![crate::bundle::manifest::ChunkEntry {
                index: 0,
                sha256: hex(&Sha256::digest(&plain)),
                compressed_bytes: 0,
                plain_bytes: plain.len() as u64,
            }],
            plain.len() as u64,
        );
        let path = write_hostile_bundle(
            "member-digest",
            &manifest,
            &[zstd::encode_all(plain.as_slice(), 3).unwrap()],
            true,
        );

        let bundle = open(&path).expect("structurally valid");
        let destination = scratch("member-digest-dest");
        let error = bundle
            .extract(&destination)
            .expect_err("a member digest mismatch must be caught");
        assert_eq!(error.kind(), crate::error::ErrorKind::DataIntegrity);
        assert!(
            error.to_string().contains("session/history.jsonl"),
            "the error must name the member: {error}"
        );
        assert!(
            !destination.join("session/history.jsonl").exists(),
            "a member failing verification must not be written"
        );
    }

    /// M11: a member span pointing past the end of the stream is refused rather
    /// than panicking on a slice out of range.
    #[test]
    fn m11_a_member_span_past_the_stream_end_is_refused() {
        let plain = b"short".to_vec();
        let manifest = base_manifest(
            vec![MemberEntry {
                path: "a.txt".into(),
                class: crate::bundle::policy::Class::SessionCritical
                    .as_str()
                    .into(),
                mode: 0o100644,
                size: 9_999,
                sha256: "0".repeat(64),
                span: crate::bundle::manifest::Span {
                    offset: 0,
                    length: 9_999,
                },
            }],
            vec![crate::bundle::manifest::ChunkEntry {
                index: 0,
                sha256: hex(&Sha256::digest(&plain)),
                compressed_bytes: 0,
                plain_bytes: plain.len() as u64,
            }],
            plain.len() as u64,
        );
        let path = write_hostile_bundle(
            "span-overrun",
            &manifest,
            &[zstd::encode_all(plain.as_slice(), 3).unwrap()],
            true,
        );

        let bundle = open(&path).unwrap();
        let error = bundle
            .extract(&scratch("span-dest"))
            .expect_err("an out-of-range span must be refused, not panic");
        assert_eq!(error.kind(), crate::error::ErrorKind::DataIntegrity);
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

    /// Session-critical members are written before reconstructible ones, so a
    /// failure part-way leaves the session usable rather than the caches
    /// restored and the history missing.
    ///
    /// Observes the order `extract()` actually writes in: the reconstructible
    /// member is listed first but carries a deliberately wrong digest, so
    /// extraction aborts on it — and the session-critical file must already be
    /// on disk. The previous version re-implemented the same sort inside the
    /// test and asserted on its own output, which was tautological: the
    /// production sort could be deleted with the test staying green (F-56
    /// class, found by the guard-test audit).
    #[test]
    fn session_critical_members_are_written_before_reconstructible_ones() {
        let (_, bundle_path) = make_bundle("ordering", &[("a.txt", "content")]);
        let mut bundle = open(&bundle_path).unwrap();

        // Hand-built manifest mixing classes: the policy cannot produce a
        // Reconstructible member from the real volume layout, so a fixture built
        // through export() could not discriminate ordering at all.
        let plain = b"CRITICAL-BYTESRECONSTRUCTIBLE".to_vec();
        bundle.chunks.clear();
        bundle
            .chunks
            .insert(0, zstd::encode_all(plain.as_slice(), 3).unwrap());
        bundle.manifest.chunks = vec![crate::bundle::manifest::ChunkEntry {
            index: 0,
            sha256: hex(&Sha256::digest(&plain)),
            compressed_bytes: 0,
            plain_bytes: plain.len() as u64,
        }];
        bundle.manifest.members = vec![
            MemberEntry {
                path: "cache/regenerable.bin".into(),
                class: crate::bundle::policy::Class::Reconstructible
                    .as_str()
                    .into(),
                mode: 0o100644,
                size: 15,
                sha256: "0".repeat(64),
                span: crate::bundle::manifest::Span {
                    offset: 14,
                    length: 15,
                },
            },
            MemberEntry {
                path: "session/history.jsonl".into(),
                class: crate::bundle::policy::Class::SessionCritical
                    .as_str()
                    .into(),
                mode: 0o100644,
                size: 14,
                sha256: hex(&Sha256::digest(b"CRITICAL-BYTES")),
                span: crate::bundle::manifest::Span {
                    offset: 0,
                    length: 14,
                },
            },
        ];

        let destination = scratch("ordering-dest");
        let error = bundle
            .extract(&destination)
            .expect_err("the corrupt reconstructible member must abort extraction");
        assert_eq!(error.kind(), crate::error::ErrorKind::DataIntegrity);

        assert!(
            destination.join("session/history.jsonl").exists(),
            "session-critical members must be written FIRST; without the production \
             sort this file would not exist when extraction aborts"
        );
    }
}
