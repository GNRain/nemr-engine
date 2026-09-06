//! Nemr engine — Phase 1 (NEMR-SPEC-001).
//!
//! Layering rule (Section 3.2, normative): `engine` depends exclusively on the
//! wrapper layer, now the standalone `nemr-containerd` crate. No code in
//! `engine` references the `containerd_client` crate directly.
//!
//! The wrapper is re-exported here as `containerd` so existing call sites read
//! `crate::containerd::…` unchanged after the crate extraction.

/// The nemrd control-plane API (E-09): the proto and generated stubs, the
/// socket path and the client, now the standalone `nemr-daemon-api` crate so
/// that a client of the daemon need not link the engine (the E-11 seam of
/// 2026-09-06). Re-exported here so existing call sites read `crate::proto::…`
/// unchanged, exactly as `containerd` is re-exported below.
pub use nemr_daemon_api::proto;

pub mod auth;
pub mod bundle;
pub mod config;
pub mod daemon;
pub mod engine;
pub mod error;
pub mod interactive;
pub mod observability;

pub use nemr_containerd as containerd;
