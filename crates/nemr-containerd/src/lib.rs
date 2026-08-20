//! Wrapper layer over the low-level `containerd-client` crate (SPEC §3.2).
//!
//! `containerd-client` exposes raw gRPC/protobuf bindings and, unlike the Go
//! client, provides no high-level convenience operations. This crate is the
//! single place that closes that gap. It is first-class infrastructure: the
//! engine and any future daemon consume it and never bypass it to call
//! `containerd-client` directly.
//!
//! # Consumer shape (E-09)
//!
//! The resolved consumption model is a long-running user daemon over a
//! Unix-domain-socket gRPC interface. This crate is designed for that consumer —
//! operations are `&self` async methods on a shared [`client::ContainerdClient`]
//! that a daemon can hold for its lifetime and share across requests — while
//! remaining usable standalone (see `src/bin/`). Nothing product-specific is
//! baked in: the cgroup prefix, image reference and naming reach the wrapper
//! through [`containers::ContainerSpec`], so the same crate serves the CLI, the
//! daemon, and the connectivity baselines without change.

pub mod client;
pub mod config;
pub mod containers;
pub mod images;
