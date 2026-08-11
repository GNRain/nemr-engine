//! Wrapper layer over the low-level `containerd-client` crate (Section 3.2).
//!
//! `containerd-client` exposes raw gRPC/protobuf bindings and, unlike the Go
//! client, provides no high-level convenience operations. This module is the
//! single place where that gap is closed. It is first-class infrastructure:
//! later milestones EXTEND it; they do not duplicate or bypass it.
//!
//! Milestone 1 surface is intentionally minimal — `list_images()` and
//! `list_containers()` only. Image pull arrives in Milestone 2; container
//! create/start in Milestones 4-5.

pub mod client;
pub mod containers;
pub mod images;
