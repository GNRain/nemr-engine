//! Milestone 1, scope item 2 — exercises the `src/containerd/` wrapper.
//!
//! Uses **only** the wrapper module: no `containerd_client` import appears
//! here. Prints byte-identical stdout to `raw_connectivity`, so AC-1.2 can be
//! demonstrated with a plain `diff` of the two programs' output.
//!
//! This is also the shape every later milestone's engine code takes: acquire a
//! `ContainerdClient`, call wrapper methods, never touch the raw crate.

use anyhow::Result;
use nemr_containerd::client::ContainerdClient;

#[tokio::main]
async fn main() -> Result<()> {
    eprintln!("[wrapper] going through src/containerd/, no direct containerd-client use");

    let client = ContainerdClient::connect().await?;

    println!("socket: {}", client.socket_path().display());
    println!("namespace: {}", client.namespace());

    let images = client.list_images().await?;
    println!("images: {}", images.len());
    for image in &images {
        println!("{}\t{}\t{}", image.name, image.digest, image.size);
    }

    let containers = client.list_containers().await?;
    println!("containers: {}", containers.len());
    for container in &containers {
        println!("{}\t{}\t{}", container.id, container.image, container.runtime);
    }

    Ok(())
}
