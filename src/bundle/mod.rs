//! Bundle export and import (M9–M11).
//!
//! Implements `docs/bundle-format.md` (NEMR-BUNDLE-001). Per E-11 the format is
//! an open, versioned public interface: `nemr export` and `nemr import` work
//! standalone against a local file, with no account and no network, and nothing
//! here may depend on the commercial sync layer.

pub mod export;
pub mod manifest;
pub mod policy;

/// Bundle schema version this build writes and can read.
///
/// A reader rejects anything greater (`Error::BundleVersionUnsupported`), which
/// is why the manifest is the archive's first member — the decision is made
/// before any content is touched.
pub const SCHEMA_VERSION: u32 = 1;
