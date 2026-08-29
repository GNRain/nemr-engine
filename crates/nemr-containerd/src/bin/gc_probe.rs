//! F-63 probe: is an unreferenced snapshot collected by containerd's GC?
//!
//! `create_container` prepares a snapshot and only afterwards writes the
//! container record that references it. Between those two calls the snapshot has
//! no lease and no referrer, which makes it garbage by containerd's definition.
//! This measures whether containerd actually collects it.
//!
//! Method: prepare a snapshot, confirm it exists, drive metadata mutations past
//! containerd's default `mutation_threshold = 100`, then look again. A control
//! snapshot is held under a lease, so "everything vanished" cannot be mistaken
//! for the effect under test.
use anyhow::{Context, Result};
use nemr_containerd::client::ContainerdClient;

const PROBE: &str = "f63-gc-probe-unreferenced";

#[tokio::main]
async fn main() -> Result<()> {
    let client = ContainerdClient::connect().await.context("connect")?;
    // No hardcoded fallback (F-124): this crate is product-agnostic and must
    // not know the engine's base image, and a literal here was one of the five
    // places a version bump had to remember. The caller says which image.
    let image = std::env::var("NEMR_PROBE_IMAGE").context(
        "set NEMR_PROBE_IMAGE to the image to probe. For the engine's base image:\n    \
         NEMR_PROBE_IMAGE=\"$(scripts/lib/base_image.sh)\" cargo run --bin gc_probe",
    )?;
    let chain_id = client
        .image_chain_id(&image)
        .await
        .context("the image must be present; build and import it first")?;

    // Clean any residue from a previous probe run.
    let _ = client.remove_snapshot(PROBE).await;

    println!("1. preparing an UNREFERENCED snapshot (exactly what create_container does)");
    client.prepare_snapshot(PROBE, &chain_id, None).await?;
    let present = client
        .list_snapshot_keys()
        .await?
        .contains(&PROBE.to_string());
    println!("   present immediately after prepare: {present}");
    anyhow::ensure!(
        present,
        "the probe snapshot was not created; the probe proves nothing"
    );

    println!("2. driving metadata mutations past containerd's default threshold (100)");
    for i in 0..130 {
        let key = format!("f63-churn-{i}");
        let _ = client.prepare_snapshot(&key, &chain_id, None).await;
        let _ = client.remove_snapshot(&key).await;
    }

    println!("3. looking for the unreferenced snapshot again");
    let survived = client
        .list_snapshot_keys()
        .await?
        .contains(&PROBE.to_string());
    println!("   present after mutation churn:      {survived}");

    let _ = client.remove_snapshot(PROBE).await;

    if survived {
        println!("\nRESULT: the snapshot SURVIVED — GC is not the mechanism.");
    } else {
        println!("\nRESULT: the snapshot was COLLECTED.");
        println!("An unreferenced snapshot does not survive containerd's GC, so the window");
        println!("between prepare_snapshot and create_container_record is a real exposure:");
        println!("create returns Ok having recorded a snapshot_key that no longer resolves,");
        println!("and the failure surfaces later at `nemr start`.");
    }
    Ok(())
}
