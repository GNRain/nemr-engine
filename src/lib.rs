//! AI Hub engine — Phase 1 (AIHUB-SPEC-001 v1.3).
//!
//! Layering rule (Section 3.2, normative): `engine` depends exclusively on
//! `containerd` (the wrapper layer). No code in `engine` may reference the
//! `containerd_client` crate directly.

pub mod auth;
pub mod config;
pub mod containerd;
pub mod engine;
