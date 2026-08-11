//! Milestone 1, scope item 1 — raw containerd connectivity baseline.
//!
//! Connects to containerd via `containerd-client` **directly, with no wrapper**,
//! and lists images and containers. This establishes that connectivity works
//! before any abstraction exists, and produces the reference output that
//! AC-1.2 compares the wrapper against.
//!
//! This is the one binary permitted to use `containerd_client` directly. It
//! must not import `ai_hub_engine::containerd` — if it did, it would no longer
//! be an independent baseline and AC-1.2 would be comparing the wrapper with
//! itself.
//!
//! The projection and sorting logic here is duplicated from the wrapper on
//! purpose, for the same reason.
//!
//! stdout carries the comparable payload; stderr carries commentary, so the
//! two programs can be compared with a plain `diff`.

use std::path::PathBuf;

use anyhow::{Context, Result};
use containerd_client::services::v1::{ListContainersRequest, ListImagesRequest};
use containerd_client::{with_namespace, Client};
use containerd_client::tonic::Request;

const NAMESPACE: &str = "default";

/// Resolve the rootless socket. Mirrors the wrapper's rules, independently.
///
/// No fallback to `/run/containerd/containerd.sock`: that is the root-owned
/// system daemon, and connecting to it would violate PRIV-01 (Section 3.7).
fn socket_path() -> Result<PathBuf> {
    if let Some(addr) = std::env::var_os("CONTAINERD_ADDRESS") {
        return Ok(PathBuf::from(addr));
    }
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
        .context("XDG_RUNTIME_DIR is not set; cannot locate the rootless containerd socket")?;
    Ok(PathBuf::from(runtime_dir)
        .join("containerd")
        .join("containerd.sock"))
}

#[tokio::main]
async fn main() -> Result<()> {
    eprintln!("[raw baseline] talking to containerd-client directly, no wrapper");

    let socket = socket_path()?;
    if !socket.exists() {
        anyhow::bail!(
            "containerd socket not found at {}. Is the rootless service running?",
            socket.display()
        );
    }

    let client = Client::from_path(&socket)
        .await
        .with_context(|| format!("failed to connect to containerd at {}", socket.display()))?;

    // Proves a server is actually answering, not merely that a socket file
    // exists and accepted a connection.
    let version = client
        .version()
        .version(())
        .await
        .context("containerd Version request failed")?;
    let version = version.get_ref();
    eprintln!(
        "[raw baseline] server responded: containerd {} (revision {})",
        version.version, version.revision
    );

    println!("socket: {}", socket.display());
    println!("namespace: {NAMESPACE}");

    // --- images ---
    let images = client
        .images()
        .list(with_namespace!(
            ListImagesRequest { filters: vec![] },
            NAMESPACE
        ))
        .await
        .context("containerd ListImages request failed")?;

    let mut images: Vec<(String, String, i64)> = images
        .into_inner()
        .images
        .into_iter()
        .map(|image| {
            let (digest, size) = image
                .target
                .map(|t| (t.digest, t.size))
                .unwrap_or_else(|| (String::new(), 0));
            (image.name, digest, size)
        })
        .collect();
    images.sort_by(|a, b| a.0.cmp(&b.0));

    println!("images: {}", images.len());
    for (name, digest, size) in &images {
        println!("{name}\t{digest}\t{size}");
    }

    // --- containers ---
    let containers = client
        .containers()
        .list(with_namespace!(
            ListContainersRequest { filters: vec![] },
            NAMESPACE
        ))
        .await
        .context("containerd ListContainers request failed")?;

    let mut containers: Vec<(String, String, String)> = containers
        .into_inner()
        .containers
        .into_iter()
        .map(|c| {
            (
                c.id,
                c.image,
                c.runtime.map(|r| r.name).unwrap_or_default(),
            )
        })
        .collect();
    containers.sort_by(|a, b| a.0.cmp(&b.0));

    println!("containers: {}", containers.len());
    for (id, image, runtime) in &containers {
        println!("{id}\t{image}\t{runtime}");
    }

    Ok(())
}
