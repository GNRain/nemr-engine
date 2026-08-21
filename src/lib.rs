//! Nemr engine — Phase 1 (NEMR-SPEC-001).
//!
//! Layering rule (Section 3.2, normative): `engine` depends exclusively on the
//! wrapper layer, now the standalone `nemr-containerd` crate. No code in
//! `engine` references the `containerd_client` crate directly.
//!
//! The wrapper is re-exported here as `containerd` so existing call sites read
//! `crate::containerd::…` unchanged after the crate extraction.

pub mod auth;
pub mod config;
pub mod engine;
pub mod error;
pub mod observability;

pub use nemr_containerd as containerd;
