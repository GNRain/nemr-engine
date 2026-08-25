//! Nemr engine — Phase 1 (NEMR-SPEC-001).
//!
//! Layering rule (Section 3.2, normative): `engine` depends exclusively on the
//! wrapper layer, now the standalone `nemr-containerd` crate. No code in
//! `engine` references the `containerd_client` crate directly.
//!
//! The wrapper is re-exported here as `containerd` so existing call sites read
//! `crate::containerd::…` unchanged after the crate extraction.

/// The nemrd control-plane gRPC service (E-09), generated from
/// `proto/nemr.proto` at build time.
pub mod proto {
    tonic::include_proto!("nemr.v1");

    /// The wire protocol version. Bump on ANY incompatible change to the
    /// service. The daemon refuses a client whose version differs (the
    /// hash-gate lesson applied to the protocol).
    pub const PROTOCOL_VERSION: u32 = 1;
}

pub mod auth;
pub mod bundle;
pub mod config;
pub mod engine;
pub mod error;
pub mod interactive;
pub mod observability;

pub use nemr_containerd as containerd;
