//! The engine's error taxonomy (E-12 / D-07).
//!
//! # Why a typed taxonomy, and why now
//!
//! Until WP D the engine used `anyhow` throughout, which is a good default for
//! an application: rich context, cheap to write, and a CLI only ever prints the
//! chain. WP D breaks that assumption in two directions at once.
//!
//! First, it introduces failure modes that do not exist yet — a partial upload,
//! a corrupt or truncated bundle, a digest mismatch, a quota exceeded on import,
//! a base image absent on the destination. Each needs a *decision* from the
//! caller, not just a message: retry, re-fetch, refuse, or prompt. `anyhow`
//! forces that decision to be made by matching on strings, which is how error
//! handling rots.
//!
//! Second, the resolved consumption model (E-09) is a daemon over gRPC. A daemon
//! must map failures to status codes, and it cannot do that from a formatted
//! string either.
//!
//! The two are independent layers, which is the point of the ruling: this enum
//! lands now so WP D's failure modes have a designed home rather than an ad-hoc
//! one, and the gRPC mapping is added at the daemon boundary later without
//! touching anything here.
//!
//! # What this is *not*
//!
//! It is not a replacement for `anyhow` everywhere. Internal plumbing that a
//! caller can only ever report keeps using `anyhow` and is wrapped at the
//! boundary. A taxonomy is only useful where someone branches on it; making
//! every internal failure a variant produces a hundred-arm enum nobody matches
//! exhaustively, which is the failure mode on the other side of this trade.
//!
//! # The classification that matters
//!
//! Every variant answers one question the caller actually has: **is this the
//! user's problem, the host's problem, or ours?** [`Error::kind`] collapses the
//! enum onto that axis, so a caller (CLI today, daemon later) can decide without
//! matching every variant — and so adding a variant does not break callers that
//! only care about the class.

use std::path::PathBuf;

/// A broad class of failure, stable across added variants.
///
/// This is what a caller branches on. The daemon will map these to gRPC status
/// codes; the CLI uses them to decide exit codes and whether to suggest a fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// The request was invalid — a bad name, an unknown size, a project that
    /// does not exist. The user changes the input and retries.
    InvalidRequest,
    /// The request was valid but conflicts with current state: the project
    /// already exists, is already running, is not running. The user changes
    /// *what* they are doing.
    Conflict,
    /// A host prerequisite is missing or misconfigured — no containerd socket,
    /// helper not installed, protocol mismatch, cgroup delegation absent. The
    /// operator fixes the machine; retrying unchanged will not help.
    HostPrerequisite,
    /// Storage is exhausted or over quota. Distinct from `HostPrerequisite`
    /// because the remedy is to free space or choose a larger quota, and
    /// distinct from `InvalidRequest` because the request was fine when made.
    CapacityExceeded,
    /// Data that should have been intact was not — a truncated bundle, a digest
    /// mismatch, a manifest that does not parse. Never retryable against the
    /// same bytes; the source must be re-fetched or is lost.
    DataIntegrity,
    /// The bundle or helper speaks a version this build does not support.
    /// Actionable in a specific way: upgrade one side or the other.
    Incompatible,
    /// A transient failure in something external — a network round trip, an
    /// object-store request. Retrying the same operation may well succeed.
    Transient,
    /// A bug in the engine, or a state the engine believes impossible. The user
    /// can do nothing except report it.
    Internal,
}

impl ErrorKind {
    /// Whether retrying the identical operation could plausibly succeed.
    ///
    /// Deliberately conservative: only `Transient` is retryable. A caller that
    /// retries a `DataIntegrity` failure against the same bytes is burning time
    /// on a certainty, and one that retries a `Conflict` will loop forever.
    pub fn is_retryable(self) -> bool {
        matches!(self, Self::Transient)
    }

    /// Whether the fix is on the operator's side (the machine) rather than the
    /// user's (the request).
    pub fn needs_operator(self) -> bool {
        matches!(self, Self::HostPrerequisite)
    }
}

/// The engine's error type.
///
/// Variants exist where a caller branches or where the message must carry
/// specific fields for a good diagnostic. Everything else arrives through
/// [`Error::Internal`] with an `anyhow` chain attached, so context is never
/// lost just because a failure did not earn its own variant.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    // --- request / state -------------------------------------------------
    #[error("invalid project name {name:?}: {reason}")]
    InvalidName { name: String, reason: String },

    #[error("no project named {name:?}\nCreate it first: nemr create {name} --size 2GB")]
    NoSuchProject { name: String },

    #[error("project {name:?} already exists\nChoose a different name, or delete the existing project first.")]
    ProjectExists { name: String },

    #[error("project {name:?} is {state}")]
    WrongState { name: String, state: &'static str },

    // --- host ------------------------------------------------------------
    #[error("{what} is not available on this host: {detail}\n{remedy}")]
    HostPrerequisite {
        what: &'static str,
        detail: String,
        remedy: String,
    },

    /// The installed privileged helper speaks a different protocol than this
    /// build expects. Its own class because the remedy is exact: reinstall.
    #[error(
        "the installed privileged helper speaks protocol {found}, but this engine requires {expected}\n\
         Reinstall it: sudo ./scripts/setup_test_host.sh"
    )]
    HelperProtocolMismatch { expected: u32, found: String },

    // --- capacity --------------------------------------------------------
    //
    // F-70: there was a second capacity variant here, `CapacityExceeded`,
    // carrying what/needed/available/context. It was constructed by nothing —
    // its `Display` had never been rendered — and it duplicated
    // `QuotaMismatch`, which IS produced, by the import capacity check. Two
    // variants for one condition, one of them unreachable, is the taxonomy
    // half-applied rather than a richer taxonomy.
    //
    // Deleted rather than wired to an invented call site. `ErrorKind::
    // CapacityExceeded` remains and is what `QuotaMismatch` maps to, so the
    // *class* a daemon switches on is unchanged.

    // --- bundle: the WP-D failure modes ----------------------------------
    /// The bundle is not readable as a bundle at all — truncated, not an
    /// archive, manifest absent or unparseable.
    #[error("bundle {path} is corrupt or truncated: {detail}")]
    BundleCorrupt { path: PathBuf, detail: String },

    /// A checksum did not match. Separate from `BundleCorrupt` because it names
    /// the specific member, which is what makes it diagnosable.
    #[error("checksum mismatch for {member} in bundle: expected {expected}, got {actual}")]
    ChecksumMismatch {
        member: String,
        expected: String,
        actual: String,
    },

    /// The bundle's schema version is outside what this build can read.
    #[error(
        "bundle schema version {found} is not supported by this engine (supports {supported})\n\
         {advice}"
    )]
    BundleVersionUnsupported {
        found: u32,
        supported: String,
        advice: String,
    },

    /// The base image the bundle references could not be resolved, and the
    /// bundle deliberately does not carry it (D-06).
    ///
    /// D-08 part 1 was scoped down deliberately: `nemr` does **not** pull from a
    /// registry, so this error must not imply it tried. The earlier draft
    /// promised to distinguish *registry unreachable* from *digest not found
    /// there* — a distinction nothing can make without attempting the fetch, and
    /// claiming it would have been a lie in the one place a user is already
    /// stuck.
    ///
    /// What it does instead: name the digest, state plainly that the image is
    /// not present locally and that nemr does not fetch images itself, and give
    /// the exact command to fetch it. An honest limit beats a message implying
    /// capability that does not exist.
    #[error(
        "the bundle was created from base image {reference}\n\
         digest: {digest}\n\
         Resolution failed. Where I looked:\n{where_looked}\n\
         {advice}"
    )]
    BaseImageUnresolved {
        reference: String,
        digest: String,
        where_looked: String,
        advice: String,
    },

    /// The destination cannot hold the bundle's contents.
    #[error(
        "the bundle needs a volume of at least {needed}, but the target project's quota is {quota}\n\
         Create the project with a larger --size and import again."
    )]
    QuotaMismatch { needed: String, quota: String },

    // --- external --------------------------------------------------------
    #[error("{operation} failed against {target}: {detail}")]
    Transient {
        operation: &'static str,
        target: String,
        detail: String,
    },

    // --- catch-all -------------------------------------------------------
    /// Anything without its own variant, with the underlying chain preserved.
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl Error {
    /// The broad class of this failure.
    pub fn kind(&self) -> ErrorKind {
        match self {
            Self::InvalidName { .. } => ErrorKind::InvalidRequest,
            Self::NoSuchProject { .. } => ErrorKind::InvalidRequest,
            Self::ProjectExists { .. } => ErrorKind::Conflict,
            Self::WrongState { .. } => ErrorKind::Conflict,
            Self::HostPrerequisite { .. } => ErrorKind::HostPrerequisite,
            Self::HelperProtocolMismatch { .. } => ErrorKind::Incompatible,
            Self::BundleCorrupt { .. } => ErrorKind::DataIntegrity,
            Self::ChecksumMismatch { .. } => ErrorKind::DataIntegrity,
            Self::BundleVersionUnsupported { .. } => ErrorKind::Incompatible,
            Self::BaseImageUnresolved { .. } => ErrorKind::HostPrerequisite,
            Self::QuotaMismatch { .. } => ErrorKind::CapacityExceeded,
            Self::Transient { .. } => ErrorKind::Transient,
            Self::Internal(_) => ErrorKind::Internal,
        }
    }

    /// Convenience for the common host-prerequisite shape.
    pub fn host(what: &'static str, detail: impl Into<String>, remedy: impl Into<String>) -> Self {
        Self::HostPrerequisite {
            what,
            detail: detail.into(),
            remedy: remedy.into(),
        }
    }
}

/// The engine's result type.
pub type Result<T, E = Error> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    /// F-70 — every `Error` variant must render a message that names its subject.
    ///
    /// Three variants (`ProjectExists`, `InvalidName`, and a now-deleted
    /// duplicate capacity variant) sat in this enum reachable by nothing, while
    /// the engine reported those exact conditions as untyped `anyhow` strings.
    /// A daemon mapping `kind()` onto a gRPC status would have answered
    /// `Internal` for what are plainly caller errors. Their `Display` had never
    /// been rendered by anything; this renders it.
    #[test]
    fn the_caller_error_variants_render_their_subject() {
        // Each with the kind a daemon must map it to. Lumping them under one
        // kind was my first version of this test, and it was wrong:
        // `ProjectExists` is a Conflict, not a bad request, and the difference
        // is the difference between 409 and 400.
        let cases = [
            (
                Error::InvalidName {
                    name: "bad/name".into(),
                    reason: "contains '/'".into(),
                },
                ErrorKind::InvalidRequest,
            ),
            (
                Error::ProjectExists {
                    name: "demo".into(),
                },
                ErrorKind::Conflict,
            ),
            (
                Error::NoSuchProject {
                    name: "demo".into(),
                },
                ErrorKind::InvalidRequest,
            ),
        ];
        for (error, expected_kind) in cases {
            let message = error.to_string();
            assert!(
                message.contains("demo") || message.contains("bad/name"),
                "a variant's message does not name its own subject: {message}"
            );
            assert_eq!(
                error.kind(),
                expected_kind,
                "a caller error misclassified; these are what a daemon turns into a \
                 status code, and none of them may be Internal: {message}"
            );
            assert_ne!(
                error.kind(),
                ErrorKind::Internal,
                "a caller error must never classify as Internal: {message}"
            );
        }
    }

    /// Every variant must map to a kind. A new variant without a `kind()` arm
    /// fails to compile, but a *wrong* arm would not — so the mapping that
    /// callers actually branch on is asserted here.
    #[test]
    fn kinds_classify_the_wp_d_failure_modes() {
        let cases: Vec<(Error, ErrorKind)> = vec![
            (
                Error::BundleCorrupt {
                    path: "/tmp/b.nemr".into(),
                    detail: "truncated at 4KiB".into(),
                },
                ErrorKind::DataIntegrity,
            ),
            (
                Error::ChecksumMismatch {
                    member: "state/history.jsonl".into(),
                    expected: "abc".into(),
                    actual: "def".into(),
                },
                ErrorKind::DataIntegrity,
            ),
            (
                Error::BundleVersionUnsupported {
                    found: 99,
                    supported: "1".into(),
                    advice: "upgrade nemr".into(),
                },
                ErrorKind::Incompatible,
            ),
            (
                Error::BaseImageUnresolved {
                    reference: "docker.io/nemr/base:0.1.0".into(),
                    digest: "sha256:abc".into(),
                    where_looked: "  - local containerd: not present".into(),
                    advice: "Build and import the base image.".into(),
                },
                ErrorKind::HostPrerequisite,
            ),
            (
                Error::QuotaMismatch {
                    needed: "3GB".into(),
                    quota: "2GB".into(),
                },
                ErrorKind::CapacityExceeded,
            ),
            (
                Error::Transient {
                    operation: "upload",
                    target: "r2://bucket/obj".into(),
                    detail: "connection reset".into(),
                },
                ErrorKind::Transient,
            ),
        ];
        for (error, expected) in cases {
            assert_eq!(error.kind(), expected, "wrong kind for {error:?}");
        }
    }

    /// Retryability is the property most likely to be got wrong by a caller, so
    /// pin it: only genuinely transient failures may be retried unchanged.
    #[test]
    fn only_transient_failures_are_retryable() {
        assert!(ErrorKind::Transient.is_retryable());
        for kind in [
            ErrorKind::InvalidRequest,
            ErrorKind::Conflict,
            ErrorKind::HostPrerequisite,
            ErrorKind::CapacityExceeded,
            ErrorKind::DataIntegrity,
            ErrorKind::Incompatible,
            ErrorKind::Internal,
        ] {
            assert!(
                !kind.is_retryable(),
                "{kind:?} must not be retryable — retrying it burns time on a certainty"
            );
        }
    }

    /// Messages must name the thing that went wrong and, where there is one, the
    /// remedy. A diagnostic that says only "checksum mismatch" is why we are
    /// building a taxonomy in the first place.
    #[test]
    fn messages_carry_specifics_and_remedies() {
        let e = Error::ChecksumMismatch {
            member: "state/history.jsonl".into(),
            expected: "aaa".into(),
            actual: "bbb".into(),
        };
        let text = e.to_string();
        assert!(
            text.contains("state/history.jsonl"),
            "names the member: {text}"
        );
        assert!(
            text.contains("aaa") && text.contains("bbb"),
            "shows both digests: {text}"
        );

        let e = Error::HelperProtocolMismatch {
            expected: 2,
            found: "1".into(),
        };
        let text = e.to_string();
        assert!(
            text.contains("setup_test_host.sh"),
            "gives the remedy: {text}"
        );
    }

    /// An anyhow chain must survive being wrapped, or the catch-all variant
    /// would lose exactly the context anyhow exists to carry.
    #[test]
    fn internal_preserves_the_anyhow_chain() {
        let root = anyhow::anyhow!("losetup failed").context("attaching the loop device");
        let error: Error = root.into();
        assert_eq!(error.kind(), ErrorKind::Internal);
        let text = format!("{:#}", anyhow::Error::from(error));
        assert!(
            text.contains("attaching the loop device"),
            "context kept: {text}"
        );
        assert!(text.contains("losetup failed"), "root cause kept: {text}");
    }
}
