//! Writing a bundle (M9).
//!
//! Implements NEMR-BUNDLE-001 §2: an uncompressed outer tar whose first member
//! is the manifest, followed by independently-compressed chunks of the
//! concatenated member stream.
//!
//! The ordering — **chunk plaintext, then compress each chunk** — is the whole
//! structural point and is easy to "optimise" away. Compressing the stream first
//! and chunking after would give a better ratio today and make M13's
//! deduplication impossible, because one changed byte early in the stream shifts
//! every downstream boundary.

use std::io::Write;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

use super::manifest::{
    BaseImageRef, ChunkEntry, ExcludedEntry, Manifest, MemberEntry, ProjectInfo, Span,
    CHUNK_PREFIX, CHUNK_SIZE, MANIFEST_MEMBER,
};
use super::policy::{Class, Decision, Policy};
use super::SCHEMA_VERSION;

/// Everything the exporter needs that it cannot discover from the volume.
pub struct ExportRequest<'a> {
    /// Project name, recorded in the manifest.
    pub project: &'a str,
    /// The size preset the project was created with.
    pub quota: &'a str,
    /// The agent that produced this session (E-15).
    pub agent: &'a str,
    /// Root to export from — the project's mounted volume.
    pub source_root: &'a Path,
    /// Base image reference and digest, referenced rather than carried (D-06).
    pub base_image: BaseImageRef,
    pub policy: Policy,
}

/// What an export produced, for the CLI to report and tests to assert on.
#[derive(Debug)]
pub struct ExportSummary {
    pub path: PathBuf,
    pub manifest: Manifest,
    pub bundle_bytes: u64,
    /// Field names dropped from `.claude.json` that this build did not
    /// recognise. Surfaced so schema drift is visible (F-54).
    pub unrecognised_fields: Vec<String>,
}

/// Write a bundle for `request` to `destination`.
pub fn export(request: &ExportRequest<'_>, destination: &Path) -> Result<ExportSummary> {
    let mut plan = plan_members(request, destination)?;

    // Accumulate the plaintext stream, recording each member's span as we go.
    // Held in memory for v1: a session bundle is megabytes (the base image is
    // referenced, not carried, and build artifacts are excluded), so streaming
    // to a temp file would add complexity for no benefit at this size. If that
    // assumption ever breaks, this is the place it breaks — and `content_bytes`
    // in the manifest is the number that would show it.
    let mut stream: Vec<u8> = Vec::new();
    let mut members: Vec<MemberEntry> = Vec::new();

    for item in &mut plan.included {
        let Content::File(path) = &item.content;
        let bytes = std::fs::read(path).map_err(|e| {
            Error::Internal(anyhow::Error::from(e).context(format!("reading {}", path.display())))
        })?;

        let offset = stream.len() as u64;
        stream.extend_from_slice(&bytes);
        members.push(MemberEntry {
            path: item.path.clone(),
            class: item.class.as_str().to_string(),
            mode: item.mode,
            size: bytes.len() as u64,
            sha256: hex(&Sha256::digest(&bytes)),
            span: Span {
                offset,
                length: bytes.len() as u64,
            },
        });
    }

    let content_bytes = stream.len() as u64;

    // Chunk the plaintext, then compress each chunk independently.
    let mut chunk_entries = Vec::new();
    let mut compressed_chunks = Vec::new();
    for (index, plain) in stream.chunks(CHUNK_SIZE).enumerate() {
        let compressed = zstd::encode_all(plain, 3).map_err(|e| {
            Error::Internal(anyhow::Error::from(e).context("compressing a bundle chunk"))
        })?;
        chunk_entries.push(ChunkEntry {
            index: index as u32,
            // Digest of the PLAINTEXT: chunk identity must not depend on the
            // codec or its level, or the same content would fail to dedupe
            // against itself once settings changed.
            sha256: hex(&Sha256::digest(plain)),
            compressed_bytes: compressed.len() as u64,
            plain_bytes: plain.len() as u64,
        });
        compressed_chunks.push(compressed);
    }

    let manifest = Manifest {
        schema_version: SCHEMA_VERSION,
        engine_version: env!("CARGO_PKG_VERSION").to_string(),
        created_at: timestamp(),
        project: ProjectInfo {
            name: request.project.to_string(),
            quota: request.quota.to_string(),
            content_bytes,
            agent: request.agent.to_string(),
        },
        base_image: request.base_image.clone(),
        chunks: chunk_entries,
        members,
        excluded: std::mem::take(&mut plan.excluded),
    };

    write_archive(destination, &manifest, &compressed_chunks)?;

    let bundle_bytes = std::fs::metadata(destination).map(|m| m.len()).unwrap_or(0);
    Ok(ExportSummary {
        path: destination.to_path_buf(),
        manifest,
        bundle_bytes,
        unrecognised_fields: plan.unrecognised_fields,
    })
}

/// Serialise the archive: manifest first, then chunks in order.
fn write_archive(destination: &Path, manifest: &Manifest, chunks: &[Vec<u8>]) -> Result<()> {
    let file = std::fs::File::create(destination).map_err(|e| {
        Error::Internal(
            anyhow::Error::from(e).context(format!("creating bundle {}", destination.display())),
        )
    })?;
    let mut archive = tar::Builder::new(file);

    let manifest_json = serde_json::to_vec_pretty(manifest).map_err(|e| {
        Error::Internal(anyhow::Error::from(e).context("serialising the bundle manifest"))
    })?;
    append(&mut archive, MANIFEST_MEMBER, &manifest_json)?;

    for (index, chunk) in chunks.iter().enumerate() {
        append(
            &mut archive,
            &format!("{CHUNK_PREFIX}{index:04}.zst"),
            chunk,
        )?;
    }

    archive
        .into_inner()
        .and_then(|mut f| f.flush())
        .map_err(|e| Error::Internal(anyhow::Error::from(e).context("finalising the bundle")))?;
    Ok(())
}

fn append<W: Write>(archive: &mut tar::Builder<W>, name: &str, bytes: &[u8]) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    // A fixed mtime keeps a bundle byte-reproducible for identical input, which
    // matters for verifying two exports of unchanged content are the same.
    header.set_mtime(0);
    header.set_cksum();
    archive
        .append_data(&mut header, name, bytes)
        .map_err(|e| Error::Internal(anyhow::Error::from(e).context(format!("writing {name}"))))
}

// ---------------------------------------------------------------------------
// Planning: what goes in, what stays out
// ---------------------------------------------------------------------------

/// Where a planned member's bytes come from.
///
/// Only real files today. A synthesised variant existed for the filtered
/// `.claude.json`, which was dead code (F-58) and has been removed with it.
enum Content {
    File(PathBuf),
}

struct PlannedMember {
    path: String,
    class: Class,
    mode: u32,
    content: Content,
}

struct Plan {
    included: Vec<PlannedMember>,
    excluded: Vec<ExcludedEntry>,
    unrecognised_fields: Vec<String>,
}

/// Walk the source root and apply the policy.
///
/// `destination` is skipped if it lies inside the source root. Without that, an
/// export written into the project's own workspace is swallowed by the *next*
/// export: the bundle grows by the size of its predecessor each time, and the
/// command reports success the whole way. Caught by the determinism test.
fn plan_members(request: &ExportRequest<'_>, destination: &Path) -> Result<Plan> {
    // Canonicalise so a relative destination, a symlinked temp dir, or `./x`
    // still matches the walked path. The bundle may not exist yet, so fall back
    // to canonicalising its parent.
    let destination_id = canonical_destination(destination);

    let mut plan = Plan {
        included: Vec::new(),
        excluded: Vec::new(),
        unrecognised_fields: Vec::new(),
    };

    let mut stack = vec![request.source_root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            // A directory that vanished mid-walk is not fatal; the manifest
            // records what was captured, and a missing member surfaces at
            // import as a checksum/member absence rather than silently.
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(relative) = path.strip_prefix(request.source_root) else {
                continue;
            };
            let relative = relative.to_string_lossy().replace('\\', "/");

            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };

            if metadata.is_dir() {
                // `lost+found` is an ext4 artefact of the volume, not project
                // content, and it is root-owned so it is unreadable anyway.
                if relative == "lost+found" {
                    continue;
                }
                match request.policy.decide(&relative) {
                    Decision::Exclude { reason } => {
                        plan.excluded.push(ExcludedEntry::new(relative, reason));
                    }
                    Decision::Include { .. } => stack.push(path),
                }
                continue;
            }
            if !metadata.is_file() {
                continue; // symlinks, sockets, devices: not session state
            }
            if destination_id
                .as_ref()
                .is_some_and(|dest| canonical_destination(&path).as_ref() == Some(dest))
            {
                continue; // never bundle the bundle we are writing
            }

            match request.policy.decide(&relative) {
                Decision::Exclude { reason } => {
                    plan.excluded.push(ExcludedEntry::new(relative, reason));
                }
                Decision::Include { class } => {
                    plan.included.push(PlannedMember {
                        path: relative,
                        class,
                        mode: mode_of(&metadata),
                        content: Content::File(path),
                    });
                }
            }
        }
    }

    // Deterministic order, so two exports of identical content produce
    // identical bundles.
    plan.included.sort_by(|a, b| a.path.cmp(&b.path));
    plan.excluded.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(plan)
}

// NOTE: `.claude.json` is deliberately NOT staged into a bundle.
//
// F-54 requires MCP configuration to travel and machine identity not to. That is
// satisfied structurally rather than by filtering: Claude Code reads
// project-scoped MCP configuration from `.mcp.json` at the project root, which
// IS the volume, so it travels as an ordinary member — while `.claude.json`
// (holding `machineID`/`oauthAccount`) stays on the rootfs and never reaches the
// exportable layer (F-55).
//
// A filtering path used to exist here, gated on the container-view path
// `root/.claude.json`, which an export walking the volume never encounters. It
// was dead code whose unit tests passed while the property they described was
// false in production (F-58), so it has been removed rather than left to look
// like a working defence.

/// Canonical identity of a path that may not exist yet.
fn canonical_destination(path: &Path) -> Option<PathBuf> {
    if let Ok(resolved) = path.canonicalize() {
        return Some(resolved);
    }
    let parent = path.parent()?.canonicalize().ok()?;
    Some(parent.join(path.file_name()?))
}

fn mode_of(metadata: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode()
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn timestamp() -> String {
    // RFC 3339 without pulling in a date library: seconds since the epoch is
    // enough to order bundles, and the manifest field is diagnostic rather than
    // load-bearing.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{secs}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nemr-export-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(root: &Path, relative: &str, contents: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    fn request<'a>(project: &'a str, root: &'a Path) -> ExportRequest<'a> {
        ExportRequest {
            project,
            quota: "2GB",
            agent: "claude-code",
            source_root: root,
            base_image: BaseImageRef {
                reference: "ghcr.io/gnrain/nemr-base:0.1.0".into(),
                digest: "sha256:deadbeef".into(),
            },
            policy: Policy::default(),
        }
    }

    /// Extract a bundle and return all file contents, for content assertions.
    ///
    /// F-57: never assert on the compressed `.nemr` bytes. A raw grep finds a
    /// plaintext marker only while the content is small enough that zstd stores
    /// it near-verbatim — measured, a marker visible in a 34-byte bundle is
    /// invisible in a 241 KiB one. A "the secret must not appear" test written
    /// that way passes on fixtures and guards nothing at real sizes.
    fn extracted_text(bundle: &Path) -> String {
        let opened = crate::bundle::import::open(bundle).expect("open bundle");
        let dir = std::env::temp_dir().join(format!(
            "nemr-xt-{}-{}",
            std::process::id(),
            bundle.file_name().unwrap().to_string_lossy()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        opened.extract(&dir).expect("extract");
        let mut combined = String::new();
        let mut stack = vec![dir.clone()];
        while let Some(current) = stack.pop() {
            for entry in std::fs::read_dir(&current).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if let Ok(text) = std::fs::read_to_string(&path) {
                    combined.push_str(&text);
                }
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
        combined
    }

    #[test]
    fn the_manifest_is_the_first_archive_member() {
        let root = scratch("first-member");
        write(&root, "workspace/notes.md", "hello");
        let out = root.join("b.nemr");
        export(&request("demo", &root), &out).unwrap();

        let mut archive = tar::Archive::new(std::fs::File::open(&out).unwrap());
        let first = archive.entries().unwrap().next().unwrap().unwrap();
        assert_eq!(
            first.path().unwrap().to_string_lossy(),
            MANIFEST_MEMBER,
            "a reader must be able to decide compatibility before reading content"
        );
    }

    /// D-02's invariant, asserted against a real bundle rather than the policy
    /// unit alone: the credential must not appear anywhere in the output.
    #[test]
    fn the_credential_never_appears_in_a_bundle() {
        let root = scratch("no-creds");
        write(&root, "workspace/notes.md", "hello");
        write(
            &root,
            "root/.claude/.credentials.json",
            r#"{"token":"SUPER-SECRET-VALUE"}"#,
        );
        let out = root.join("b.nemr");
        let summary = export(&request("demo", &root), &out).unwrap();

        assert!(
            !summary
                .manifest
                .members
                .iter()
                .any(|m| m.path.contains("credentials")),
            "no member may reference the credential"
        );
        assert!(
            !extracted_text(&out).contains("SUPER-SECRET-VALUE"),
            "the secret must not appear in the bundle's extracted content (D-02)"
        );
        assert!(
            summary
                .manifest
                .excluded
                .iter()
                .any(|e| e.path.contains("credentials") && e.reason == "secret"),
            "and the exclusion must be recorded, not silent"
        );
    }

    #[test]
    fn build_artifacts_are_excluded_and_recorded() {
        let root = scratch("artifacts");
        write(&root, "workspace/src/main.rs", "fn main() {}");
        write(&root, "workspace/target/debug/app", "BINARY");
        let out = root.join("b.nemr");
        let summary = export(&request("demo", &root), &out).unwrap();

        let paths: Vec<&str> = summary
            .manifest
            .members
            .iter()
            .map(|m| m.path.as_str())
            .collect();
        assert!(paths.contains(&"workspace/src/main.rs"));
        assert!(
            !paths.iter().any(|p| p.contains("target/")),
            "build output must not travel: {paths:?}"
        );
        assert!(summary
            .manifest
            .excluded
            .iter()
            .any(|e| e.path.contains("target")));
    }

    /// Chunk digests are over plaintext, so chunk identity does not depend on
    /// the codec — the property M13's deduplication rests on.
    #[test]
    fn chunk_digests_are_over_plaintext_not_compressed_bytes() {
        let root = scratch("chunk-digest");
        let body = "x".repeat(1000);
        write(&root, "workspace/a.txt", &body);
        let out = root.join("b.nemr");
        let summary = export(&request("demo", &root), &out).unwrap();

        let chunk = &summary.manifest.chunks[0];
        assert_eq!(
            chunk.sha256,
            hex(&Sha256::digest(body.as_bytes())),
            "the digest must be of the plaintext chunk"
        );
        assert!(
            chunk.compressed_bytes < chunk.plain_bytes,
            "and the stored chunk is compressed: {} vs {}",
            chunk.compressed_bytes,
            chunk.plain_bytes
        );
    }

    #[test]
    fn content_bytes_lets_import_check_quota_before_extracting() {
        let root = scratch("content-bytes");
        write(&root, "workspace/a.txt", "12345");
        write(&root, "workspace/b.txt", "678");
        let out = root.join("b.nemr");
        let summary = export(&request("demo", &root), &out).unwrap();
        assert_eq!(summary.manifest.project.content_bytes, 8);
    }

    /// Two exports of identical content must produce identical bytes, or
    /// "did anything change?" cannot be answered cheaply.
    #[test]
    fn export_is_deterministic_for_unchanged_content() {
        let root = scratch("determinism");
        let out_dir = scratch("determinism-out");
        write(&root, "workspace/a.txt", "stable");
        write(&root, "workspace/nested/b.txt", "also stable");

        let mut a = export(&request("demo", &root), &out_dir.join("first.nemr")).unwrap();
        let mut b = export(&request("demo", &root), &out_dir.join("second.nemr")).unwrap();

        // created_at is a wall-clock stamp and is expected to differ.
        a.manifest.created_at = String::new();
        b.manifest.created_at = String::new();
        assert_eq!(
            a.manifest, b.manifest,
            "manifests must match but for the timestamp"
        );
    }

    /// Exporting into the project's own workspace must not swallow the bundle
    /// being written, nor a previous one on the next run.
    ///
    /// Found by the determinism test: the second export included the first
    /// bundle as a member. In use that means a bundle that grows by the size of
    /// its predecessor every time, while the command reports success — the
    /// silent-wrong-result shape this project keeps hitting.
    #[test]
    fn exporting_into_the_source_root_does_not_swallow_the_bundle() {
        let root = scratch("self-swallow");
        write(&root, "workspace/a.txt", "content");

        let inside = root.join("workspace/backup.nemr");
        let first = export(&request("demo", &root), &inside).unwrap();
        assert!(
            !first
                .manifest
                .members
                .iter()
                .any(|m| m.path.ends_with(".nemr")),
            "the bundle must not contain itself: {:?}",
            first
                .manifest
                .members
                .iter()
                .map(|m| &m.path)
                .collect::<Vec<_>>()
        );

        // And a second export must not pick up the first one either.
        let second = export(
            &request("demo", &root),
            &root.join("workspace/backup2.nemr"),
        )
        .unwrap();
        let swallowed: Vec<&String> = second
            .manifest
            .members
            .iter()
            .map(|m| &m.path)
            .filter(|p| p.ends_with(".nemr"))
            .collect();
        assert_eq!(
            swallowed,
            Vec::<&String>::new(),
            "a previous bundle must not be swallowed by the next export"
        );
    }

    #[test]
    fn member_spans_address_the_concatenated_stream() {
        let root = scratch("spans");
        write(&root, "workspace/a.txt", "AAAA");
        write(&root, "workspace/b.txt", "BB");
        let out = root.join("b.nemr");
        let summary = export(&request("demo", &root), &out).unwrap();

        let total: u64 = summary.manifest.members.iter().map(|m| m.span.length).sum();
        assert_eq!(total, summary.manifest.project.content_bytes);
        // Spans must tile the stream without overlapping.
        let mut spans: Vec<Span> = summary.manifest.members.iter().map(|m| m.span).collect();
        spans.sort_by_key(|s| s.offset);
        let mut cursor = 0;
        for span in spans {
            assert_eq!(span.offset, cursor, "spans must be contiguous");
            cursor += span.length;
        }
    }
}
