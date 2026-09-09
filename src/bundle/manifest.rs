//! The bundle manifest (NEMR-BUNDLE-001 §3).
//!
//! The manifest is the archive's **first member**, so a reader decides whether
//! it understands a bundle before touching a byte of content. Everything a
//! reader needs to refuse early — schema version, base image digest, total
//! content size — lives here rather than being discovered mid-extraction.

use serde::{Deserialize, Serialize};

use super::policy::{Class, ExcludeReason};

/// Archive member name of the manifest. Always written first.
pub const MANIFEST_MEMBER: &str = "manifest.json";

/// Directory prefix for chunk members inside the archive.
pub const CHUNK_PREFIX: &str = "chunks/";

/// Fixed chunk size for schema v1.
///
/// v1 chunks at fixed boundaries; M13 swaps in content-defined chunking (a
/// rolling hash) and changes only *how boundaries are chosen*, not the layout,
/// the manifest shape, or the reader. That is the seam the uncompressed outer
/// archive exists to preserve.
pub const CHUNK_SIZE: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    /// Bundle schema version. A reader rejects anything it does not support
    /// before reading content.
    pub schema_version: u32,
    /// Engine build that produced this bundle. Diagnostic only — compatibility
    /// is decided by `schema_version`, never by this.
    pub engine_version: String,
    /// RFC 3339 timestamp.
    pub created_at: String,
    pub project: ProjectInfo,
    pub base_image: BaseImageRef,
    pub chunks: Vec<ChunkEntry>,
    pub members: Vec<MemberEntry>,
    /// What did not travel, and why. Explicit so the far side can answer
    /// "where did my X go?" without guessing.
    pub excluded: Vec<ExcludedEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectInfo {
    pub name: String,
    /// The size preset the source project used, e.g. `2GB`.
    pub quota: String,
    /// Total uncompressed bytes of included members.
    ///
    /// Present so import can check the destination quota **before** extracting
    /// anything, rather than failing half-way through (`Error::QuotaMismatch`).
    pub content_bytes: u64,

    /// The agent that PRODUCED this session (E-15).
    ///
    /// Named for what it records, not "the project's agent": a session has one
    /// author, and cross-agent migration (a future epic) will read another
    /// agent's transcript and re-emit it, at which point "produced by" and "will
    /// run under" diverge. Recording the producer now leaves room for that
    /// without a schema change.
    ///
    /// `#[serde(default)]` makes this v1-compatible in BOTH directions,
    /// established empirically (see the round-trip test): an old reader ignores
    /// it, and this reader treats its absence as Claude Code — which is correct,
    /// because every bundle written before this field existed was Claude Code.
    #[serde(default = "default_agent_id")]
    pub agent: String,
}

/// The agent id assumed when a manifest predates the `agent` field.
fn default_agent_id() -> String {
    crate::engine::agent::Agent::default_agent()
        .id()
        .to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BaseImageRef {
    /// Human-readable reference. A hint: tags move, so this is not authoritative.
    pub reference: String,
    /// The authoritative identity. Import refuses on mismatch rather than
    /// substituting a different rootfs — a session restored onto the wrong base
    /// image is a silent-wrong-result defect.
    pub digest: String,
    /// The rootfs CHAIN ID this project was actually built on (F-115).
    ///
    /// `reference` and `digest` above were, until this field existed, both taken
    /// from the engine's `BASE_IMAGE` constant rather than from the container —
    /// so they recorded what the exporting engine builds *today*, not what the
    /// project runs on. Usually the same; silently wrong when they differ, which
    /// is the one failure class this field exists to prevent.
    ///
    /// The chain id is the only identity that always survives. A reference can
    /// be retagged (F-85 reused a tag on this project's own history), an image
    /// can be removed, and a stale reference can name a squattable namespace
    /// (F-25). The chain id is fixed when the snapshot is prepared at create
    /// time and stays readable afterwards regardless.
    ///
    /// `#[serde(default)]` makes it v1-compatible in both directions, the same
    /// way `agent` above is: an old reader ignores it, and this reader treats
    /// its absence as "the bundle predates this field", falling back to the
    /// digest check. **A v1 reader therefore gets the older, weaker guarantee —
    /// that is a real limit, not a rounding error.**
    #[serde(default)]
    pub rootfs_chain_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChunkEntry {
    pub index: u32,
    /// SHA-256 of the **plaintext** chunk, not the compressed member.
    ///
    /// Plaintext is what M13 deduplicates on and what v2 encrypts; hashing the
    /// compressed bytes would tie the identity of a chunk to the codec and its
    /// settings, so the same content would dedupe against itself only if
    /// compressed identically.
    pub sha256: String,
    pub compressed_bytes: u64,
    pub plain_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemberEntry {
    /// Path relative to the export root, forward slashes, no leading slash.
    pub path: String,
    /// Session-critical or reconstructible, from C1's measurements. This is
    /// what makes lazy materialisation possible at import.
    pub class: String,
    /// Unix mode bits.
    pub mode: u32,
    /// Modification time, seconds since the epoch (F-20).
    ///
    /// Claude Code's resume list sorts transcripts by mtime, so a bundle that
    /// drops them lands every session on the same age and the list loses its
    /// order — with a real history the user cannot tell which session they were
    /// last in. Optional so a bundle written before F-20 still imports: absent
    /// means "leave whatever the extraction produced".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtime: Option<i64>,
    pub size: u64,
    pub sha256: String,
    /// Where the member's bytes live in the chunk stream.
    pub span: Span,
}

impl MemberEntry {
    pub fn is_session_critical(&self) -> bool {
        self.class == Class::SessionCritical.as_str()
    }
}

/// A byte range in the concatenated chunk stream.
///
/// A member may span multiple chunks, so this is an offset into the *stream*
/// rather than into one chunk — which is what lets chunk boundaries be chosen
/// independently of member boundaries (required for content-defined chunking).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct Span {
    /// Absolute offset into the concatenated plaintext chunk stream.
    pub offset: u64,
    pub length: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExcludedEntry {
    pub path: String,
    pub reason: String,
}

impl ExcludedEntry {
    pub fn new(path: impl Into<String>, reason: ExcludeReason) -> Self {
        Self {
            path: path.into(),
            reason: reason.as_str().to_string(),
        }
    }
}

impl Manifest {
    /// Members that must be present before a user can attach.
    pub fn session_critical(&self) -> impl Iterator<Item = &MemberEntry> {
        self.members.iter().filter(|m| m.is_session_critical())
    }

    /// Whether this build can read the bundle, and why not if it cannot.
    ///
    /// Checked before any content is read. Older bundles are readable; newer
    /// ones are not, because a future version may change semantics this build
    /// cannot know about — refusing is the only safe direction.
    pub fn compatibility(&self) -> Result<(), crate::error::Error> {
        if self.schema_version > super::SCHEMA_VERSION {
            return Err(crate::error::Error::BundleVersionUnsupported {
                found: self.schema_version,
                supported: super::SCHEMA_VERSION.to_string(),
                advice: "This bundle was written by a newer engine. Upgrade nemr and retry."
                    .to_string(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `agent` field is v1-compatible in BOTH directions (E-15).
    ///
    /// Forward: a manifest written before the field existed (no `agent` key)
    /// must read back as Claude Code, because every such bundle was Claude Code.
    /// Backward: a manifest carrying the field must round-trip unchanged. This
    /// is why no schema bump was needed — established here, not assumed.
    #[test]
    fn the_agent_field_is_v1_compatible_both_ways() {
        // Forward: JSON with no `agent` key at all (a genuine pre-field bundle).
        let without = r#"{"name":"old","quota":"2GB","content_bytes":10}"#;
        let info: ProjectInfo =
            serde_json::from_str(without).expect("old manifest must still parse");
        assert_eq!(
            info.agent, "claude-code",
            "a manifest predating the agent field must read back as Claude Code"
        );

        // Backward: a manifest with the field round-trips.
        let with = ProjectInfo {
            name: "new".into(),
            quota: "500MB".into(),
            content_bytes: 20,
            agent: "codex".into(),
        };
        let json = serde_json::to_string(&with).expect("serialize");
        let back: ProjectInfo = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(
            back, with,
            "a manifest with the agent field must round-trip unchanged"
        );

        // And an OLD reader (one that does not know `agent`) ignores it rather
        // than rejecting — modelled by a struct without the field, since serde
        // ignores unknown fields by default (no deny_unknown_fields).
        #[derive(serde::Deserialize)]
        struct OldProjectInfo {
            name: String,
            #[allow(dead_code)]
            quota: String,
        }
        let old: OldProjectInfo =
            serde_json::from_str(&json).expect("an old reader must accept a manifest with agent");
        assert_eq!(old.name, "new");
    }

    /// The `rootfs_chain_id` field is v1-compatible in BOTH directions (F-115).
    ///
    /// Forward: a bundle written before the field existed reads back with an
    /// EMPTY chain id, which import treats as "this bundle cannot tell me what
    /// it ran on" and falls back to the digest check — the guarantee those
    /// bundles were written under. Backward: a manifest carrying it round-trips,
    /// and a reader that does not know the field ignores it.
    ///
    /// The limit, asserted rather than implied: an old reader silently gets the
    /// WEAKER check. It cannot enforce a rootfs identity it cannot see.
    #[test]
    fn the_rootfs_chain_id_field_is_v1_compatible_both_ways() {
        // Forward: a genuine pre-field base_image object.
        let without = r#"{"reference":"ghcr.io/gnrain/nemr-base:0.2.0","digest":"sha256:aaa"}"#;
        let old_ref: BaseImageRef =
            serde_json::from_str(without).expect("a pre-field manifest must still parse");
        assert_eq!(
            old_ref.rootfs_chain_id, "",
            "a bundle predating the field must read back with no chain id, so import falls \
             back to the digest check rather than refusing"
        );

        // Backward: round-trips unchanged.
        let with = BaseImageRef {
            reference: "ghcr.io/gnrain/nemr-base:0.2.0".into(),
            digest: "sha256:39c5ade9".into(),
            rootfs_chain_id: "sha256:9326c0f8".into(),
        };
        let json = serde_json::to_string(&with).expect("serialize");
        let back: BaseImageRef = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, with);

        // An OLD reader ignores it rather than rejecting.
        #[derive(serde::Deserialize)]
        struct OldBaseImageRef {
            reference: String,
            digest: String,
        }
        let old: OldBaseImageRef =
            serde_json::from_str(&json).expect("an old reader must accept the new field");
        assert_eq!(old.digest, "sha256:39c5ade9");
        assert_eq!(old.reference, "ghcr.io/gnrain/nemr-base:0.2.0");
    }

    /// An unidentifiable base image records NO digest rather than a plausible
    /// one. Recording the engine's current constant as though it were the
    /// project's identity is the defect F-115 fixes, and an empty digest is what
    /// makes "I could not prove this" distinguishable from "I proved this".
    #[test]
    fn an_unproven_base_image_records_no_digest() {
        let unproven = BaseImageRef {
            reference: "ghcr.io/gnrain/nemr-base:0.2.0".into(),
            digest: String::new(),
            rootfs_chain_id: "sha256:66ba6d20".into(),
        };
        assert!(
            unproven.digest.is_empty() && !unproven.rootfs_chain_id.is_empty(),
            "the chain id is always knowable; the digest is only knowable when a local \
             image builds that rootfs"
        );
    }

    fn manifest_with_version(schema_version: u32) -> Manifest {
        Manifest {
            schema_version,
            engine_version: "0.1.0".into(),
            created_at: "2026-08-21T00:00:00Z".into(),
            project: ProjectInfo {
                name: "demo".into(),
                quota: "2GB".into(),
                content_bytes: 0,
                agent: "claude-code".into(),
            },
            base_image: BaseImageRef {
                reference: "ghcr.io/gnrain/nemr-base:0.2.0".into(),
                digest: "sha256:abc".into(),
                rootfs_chain_id: String::new(),
            },
            chunks: vec![],
            members: vec![],
            excluded: vec![],
        }
    }

    #[test]
    fn a_newer_schema_is_refused_with_advice() {
        let error = manifest_with_version(super::super::SCHEMA_VERSION + 1)
            .compatibility()
            .expect_err("a newer bundle must be refused");
        assert_eq!(error.kind(), crate::error::ErrorKind::Incompatible);
        let text = error.to_string();
        assert!(text.contains("Upgrade nemr"), "must say what to do: {text}");
    }

    #[test]
    fn the_current_schema_is_accepted() {
        assert!(manifest_with_version(super::super::SCHEMA_VERSION)
            .compatibility()
            .is_ok());
    }

    /// The manifest is a public interface (E-11), so its serialised field names
    /// are part of the contract. A rename is a breaking change and must be
    /// caught here rather than by a user's failed import.
    #[test]
    fn serialised_field_names_are_the_public_contract() {
        let json = serde_json::to_value(manifest_with_version(1)).unwrap();
        for field in [
            "schema_version",
            "engine_version",
            "created_at",
            "project",
            "base_image",
            "chunks",
            "members",
            "excluded",
        ] {
            assert!(json.get(field).is_some(), "manifest must carry `{field}`");
        }
        assert!(
            json["project"].get("content_bytes").is_some(),
            "content_bytes is what lets import check quota before extracting"
        );
        assert!(
            json["base_image"].get("digest").is_some(),
            "digest is the authoritative base image identity"
        );
    }

    #[test]
    fn round_trips_through_json() {
        let original = manifest_with_version(1);
        let text = serde_json::to_string(&original).unwrap();
        let parsed: Manifest = serde_json::from_str(&text).unwrap();
        assert_eq!(original, parsed);
    }

    #[test]
    fn session_critical_members_are_selectable_for_lazy_materialisation() {
        let mut manifest = manifest_with_version(1);
        manifest.members = vec![
            MemberEntry {
                path: "workspace/notes.md".into(),
                class: Class::SessionCritical.as_str().into(),
                mode: 0o100644,
                mtime: None,
                size: 10,
                sha256: "a".into(),
                span: Span {
                    offset: 0,
                    length: 10,
                },
            },
            MemberEntry {
                path: "root/.claude/backups/x".into(),
                class: Class::Reconstructible.as_str().into(),
                mode: 0o100644,
                mtime: None,
                size: 5,
                sha256: "b".into(),
                span: Span {
                    offset: 10,
                    length: 5,
                },
            },
        ];
        let critical: Vec<&str> = manifest
            .session_critical()
            .map(|m| m.path.as_str())
            .collect();
        assert_eq!(critical, vec!["workspace/notes.md"]);
    }
}
