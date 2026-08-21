//! Object-storage seam for bundle sync — the **commercial** side of E-11.
//!
//! # What lives here and why
//!
//! E-11 puts the engine, the containerd wrapper, the volume layer, the
//! privileged helper and the bundle *format specification* on the open side;
//! the sync layer, the lease service, cloud storage backends and identity on the
//! commercial side. This crate is the second list.
//!
//! The boundary is kept by **dependency direction**, not by convention: this
//! crate may depend on the open format, and `nemr-engine` must never depend on
//! this crate. If it did, `nemr export` and `nemr import` would inherit an
//! object-store dependency and stop working on a machine with no network and no
//! account — which is precisely what
//! `e11_export_and_import_work_with_no_network_and_no_credentials` asserts and
//! what `scripts/check_seam.sh` enforces mechanically.
//!
//! # Egress-consciousness is a design constraint, not a tuning detail (D-05)
//!
//! Every attach on a new machine is a download, so egress cost scales with
//! active usage rather than with stored bytes. R2 is chosen first because its
//! egress is free; B2 stays viable behind the same interface because its free
//! egress is capped at a multiple of stored bytes and a sync product with small
//! bundles and frequent pulls can exceed that.
//!
//! The trait is shaped by that: it exposes `head` so a caller can check
//! existence and size **without transferring the object**, and `get_range` so a
//! caller can fetch a bundle's manifest without downloading its chunks. Both
//! exist so the common paths — "is this already here?", "what does this bundle
//! contain?" — cost no egress. A trait that only offered `get` would make those
//! questions expensive and the cost would be invisible until the bill arrived.

use std::fmt;

pub mod conformance;
pub mod local;
pub mod s3;

/// Where an object lives within a store. Opaque, `/`-separated.
///
/// Deliberately not a `Path`: object stores have no directories, and treating
/// keys as filesystem paths is how `..` and absolute-path bugs get in. A key is
/// validated on construction and is a plain string thereafter.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectKey(String);

impl ObjectKey {
    /// Build a key, rejecting anything that could escape a prefix or confuse a
    /// backend.
    ///
    /// Object stores accept far more than is safe to round-trip through a
    /// filesystem-backed implementation, so the strictest common denominator is
    /// applied here rather than per backend — otherwise a key that works against
    /// R2 could traverse against the local store used in tests.
    pub fn new(key: impl Into<String>) -> Result<Self, StorageError> {
        let key = key.into();
        if key.is_empty() {
            return Err(StorageError::InvalidKey {
                key,
                reason: "must not be empty".into(),
            });
        }
        if key.starts_with('/') || key.ends_with('/') {
            return Err(StorageError::InvalidKey {
                key,
                reason: "must not start or end with '/'".into(),
            });
        }
        if key.split('/').any(|segment| {
            segment.is_empty() || segment == "." || segment == ".." || segment.contains('\\')
        }) {
            return Err(StorageError::InvalidKey {
                key,
                reason: "segments must be non-empty and must not be '.', '..' or contain '\\'".into(),
            });
        }
        if key.contains('\0') {
            return Err(StorageError::InvalidKey {
                key,
                reason: "must not contain a NUL".into(),
            });
        }
        Ok(Self(key))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ObjectKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What `head` reports: everything answerable without transferring the object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectMeta {
    pub key: ObjectKey,
    pub size: u64,
    /// Backend-supplied entity tag, when the backend provides one. Advisory:
    /// S3-compatible stores differ on whether this is an MD5, so it is never
    /// used as an integrity check — bundle integrity comes from the manifest's
    /// SHA-256 digests, which are computed by the open half.
    pub etag: Option<String>,
}

/// Failures a store can produce.
///
/// Mirrors the engine's `ErrorKind` split — is this the caller's problem, the
/// service's, or ours — so a daemon can map both to gRPC status codes with one
/// policy rather than two.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("invalid object key {key:?}: {reason}")]
    InvalidKey { key: String, reason: String },

    #[error("no object at {key}")]
    NotFound { key: ObjectKey },

    #[error("access denied for {key}: {detail}")]
    AccessDenied { key: ObjectKey, detail: String },

    /// The request failed in a way that may succeed if retried — a timeout, a
    /// reset connection, a 5xx. Distinguished so a sync loop can retry these and
    /// only these.
    #[error("{operation} failed for {key} (retryable): {detail}")]
    Transient {
        operation: &'static str,
        key: ObjectKey,
        detail: String,
    },

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl StorageError {
    /// Whether retrying the identical request could plausibly succeed.
    ///
    /// Conservative, like the engine's taxonomy: only `Transient`. Retrying a
    /// `NotFound` or an `AccessDenied` burns egress and time on a certainty.
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Transient { .. })
    }
}

pub type Result<T, E = StorageError> = std::result::Result<T, E>;

/// An S3-compatible object store.
///
/// Implemented by R2 and B2 behind the same interface, and by
/// [`local::LocalStore`] for tests and for a self-hoster moving bundles between
/// their own machines. **No vendor-specific behaviour may appear above this
/// trait** (E-11): if a method's contract can only be satisfied by one provider,
/// it does not belong here.
#[allow(async_fn_in_trait)]
pub trait ObjectStore: Send + Sync {
    /// Human-readable identity, for diagnostics only. Never branched on —
    /// branching on it is how vendor-specific behaviour leaks upward.
    fn describe(&self) -> String;

    /// Metadata without transferring the object (D-05: costs no egress).
    async fn head(&self, key: &ObjectKey) -> Result<ObjectMeta>;

    /// Fetch an entire object.
    async fn get(&self, key: &ObjectKey) -> Result<Vec<u8>>;

    /// Fetch a byte range.
    ///
    /// The reason a bundle's manifest is its first archive member: a caller can
    /// read the manifest, decide compatibility, and list contents while
    /// transferring kilobytes rather than the whole bundle. `range` is
    /// half-open, and a backend must clamp it to the object's size rather than
    /// erroring, so a caller need not `head` first.
    async fn get_range(&self, key: &ObjectKey, range: std::ops::Range<u64>) -> Result<Vec<u8>>;

    /// Store an object, replacing any existing one.
    async fn put(&self, key: &ObjectKey, bytes: &[u8]) -> Result<()>;

    /// Remove an object. Absent is success — deletion is idempotent, so a
    /// retried cleanup after a partial failure does not fail.
    async fn delete(&self, key: &ObjectKey) -> Result<()>;

    /// Keys under a prefix, sorted.
    async fn list(&self, prefix: &str) -> Result<Vec<ObjectKey>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_that_could_escape_a_prefix_are_refused() {
        for bad in [
            "",
            "/leading",
            "trailing/",
            "a//b",
            "a/../b",
            "..",
            ".",
            "a/./b",
            "a\\b",
            "with\0nul",
        ] {
            assert!(
                ObjectKey::new(bad).is_err(),
                "{bad:?} must be refused: a key that escapes its prefix in the local \
                 backend is a traversal, the same class as the bundle-member one"
            );
        }
    }

    #[test]
    fn ordinary_keys_are_accepted() {
        for good in [
            "bundle.nemr",
            "projects/demo/2026-08-21.nemr",
            "a/b/c/d.bin",
            "with-dash_and.dot",
        ] {
            assert!(ObjectKey::new(good).is_ok(), "{good:?} should be accepted");
        }
    }

    #[test]
    fn only_transient_failures_are_retryable() {
        let key = ObjectKey::new("x").unwrap();
        assert!(StorageError::Transient {
            operation: "get",
            key: key.clone(),
            detail: "reset".into()
        }
        .is_retryable());

        for error in [
            StorageError::NotFound { key: key.clone() },
            StorageError::AccessDenied {
                key,
                detail: "403".into(),
            },
        ] {
            assert!(
                !error.is_retryable(),
                "{error:?} must not be retryable — retrying burns egress on a certainty"
            );
        }
    }
}
