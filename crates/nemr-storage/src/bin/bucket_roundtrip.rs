//! Round-trip a real bundle against a real bucket (M12 acceptance).
//!
//! Separated into its own binary because it is the one step that needs a live
//! credential, and the person holding that credential is not the person writing
//! the code. It reads configuration from the environment, exercises every trait
//! method against the bucket, and cleans up after itself.
//!
//! It prints only what it did — never a credential, never a URL with a query
//! string. See `S3Config`'s `Debug`, which redacts both halves of the key.
//!
//!   NEMR_S3_PROVIDER=r2 \
//!   NEMR_S3_BUCKET=<bucket> \
//!   NEMR_S3_ENDPOINT=https://<account>.r2.cloudflarestorage.com \
//!   NEMR_S3_ACCESS_KEY_ID=<id> \
//!   NEMR_S3_SECRET_ACCESS_KEY=<secret> \
//!   cargo run -p nemr-storage --bin bucket_roundtrip -- <bundle.nemr>
//!
//! Exit 0 means the backend round-tripped the bundle byte-identically.

use std::process::ExitCode;

use nemr_storage::{s3, ObjectKey, ObjectStore};

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("bucket_roundtrip: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> anyhow::Result<()> {
    let bundle_path = std::env::args().nth(1).ok_or_else(|| {
        anyhow::anyhow!(
            "usage: bucket_roundtrip <bundle.nemr>\n\
             Export one first: nemr export <project> -o /tmp/bundle.nemr"
        )
    })?;

    let bundle = std::fs::read(&bundle_path)
        .map_err(|e| anyhow::anyhow!("cannot read {bundle_path}: {e}"))?;
    println!("bundle: {bundle_path} ({} bytes)", bundle.len());

    let config = s3::S3Config::from_env()?.ok_or_else(|| {
        anyhow::anyhow!(
            "no backend configured. Set NEMR_S3_BUCKET, NEMR_S3_ENDPOINT, \
             NEMR_S3_ACCESS_KEY_ID and NEMR_S3_SECRET_ACCESS_KEY."
        )
    })?;
    // Safe to print: Debug redacts both halves of the credential.
    println!("config: {config:?}");

    let store = s3::S3Store::new(&config)?;
    println!("store:  {}", store.describe());

    // A unique key per run, so a failed run never collides with a later one and
    // so this can never overwrite a real object in a shared bucket.
    let key = ObjectKey::new(format!(
        "nemr-roundtrip/{}-{}.nemr",
        std::process::id(),
        bundle.len()
    ))?;
    println!("key:    {key}");

    // Clean up even if an assertion fails partway, so a scoped test token is not
    // left holding an object the operator has to find by hand.
    let outcome = nemr_storage::conformance::round_trip(&store, &key, &bundle).await;

    print!("cleanup … ");
    match store.delete(&key).await {
        Ok(()) => println!("deleted {key}"),
        Err(e) => println!("FAILED to delete {key}: {e}  (remove it manually)"),
    }

    outcome?;
    println!("\nPASS — the bundle round-tripped byte-identically against a real bucket.");
    Ok(())
}

