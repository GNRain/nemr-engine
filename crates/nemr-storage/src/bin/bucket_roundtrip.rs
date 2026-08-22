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
//!   cargo run -p nemr-storage --bin bucket_roundtrip -- [--keep] <bundle.nemr>
//!
//! Exit 0 means the backend round-tripped the bundle byte-identically.
//!
//! # `--keep`, and why it exists (F-61)
//!
//! By default the run deletes what it wrote, on every exit path including
//! assertion failure — which leaves an operator nothing to inspect, so accepting
//! the result means trusting this tool's own summary. In a project whose
//! standing rule is that a green signal must be provable, the acceptance harness
//! should not be the one thing taken on faith.
//!
//! `--keep` skips the cleanup and prints the key, size and server ETag beside
//! the locally computed SHA-256, so the object can be found in the provider's
//! console, downloaded, and hashed independently. It verifies strictly less than
//! a default run (delete-idempotence is skipped), and says so.
//!
//! # `--local <dir>`
//!
//! Runs the identical acceptance against a directory instead of a bucket, so
//! this binary can be exercised end-to-end with no credential. That is how the
//! tool is checked before it is trusted with a live one: a green `--local` run
//! says the harness works; only a green bucket run says the bucket does. The
//! output labels which claim is being made.

use std::process::ExitCode;

use nemr_storage::conformance::{run_acceptance, Cleanup, RoundTripReport};
use nemr_storage::{local::LocalStore, s3, ObjectKey, ObjectStore};

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
    const USAGE: &str = "usage: bucket_roundtrip [--keep] [--local <dir>] <bundle.nemr>";

    let mut keep = false;
    let mut local_dir: Option<String> = None;
    let mut bundle_path: Option<String> = None;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--keep" => keep = true,
            "--local" => {
                local_dir = Some(
                    arguments
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--local needs a directory. {USAGE}"))?,
                )
            }
            other if other.starts_with("--") => {
                anyhow::bail!("unknown flag {other}. {USAGE}")
            }
            other => bundle_path = Some(other.to_string()),
        }
    }
    let bundle_path = bundle_path.ok_or_else(|| {
        anyhow::anyhow!(
            "{USAGE}\n\
             Export one first: nemr export <project> -o /tmp/bundle.nemr"
        )
    })?;
    let cleanup = if keep { Cleanup::Keep } else { Cleanup::Remove };

    let bundle = std::fs::read(&bundle_path)
        .map_err(|e| anyhow::anyhow!("cannot read {bundle_path}: {e}"))?;
    println!("bundle: {bundle_path} ({} bytes)", bundle.len());

    // A unique key per run, so a failed run never collides with a later one and
    // so this can never overwrite a real object in a shared bucket.
    let key = ObjectKey::new(format!(
        "nemr-roundtrip/{}-{}.nemr",
        std::process::id(),
        bundle.len()
    ))?;
    println!("key:    {key}");

    // The same acceptance either way — the only difference is what it is
    // pointed at, and which claim a green run supports.
    let (report, location) = match &local_dir {
        Some(dir) => {
            let store = LocalStore::new(std::path::Path::new(dir));
            println!(
                "store:  {} (NO credential — this checks the harness, not a bucket)",
                store.describe()
            );
            (
                run_acceptance(&store, &key, &bundle, cleanup).await?,
                format!("{dir}/{key}"),
            )
        }
        None => run_against_bucket(&key, &bundle, cleanup).await?,
    };

    match &local_dir {
        Some(_) => println!(
            "\nPASS — the acceptance harness round-trips a bundle byte-identically.\n\
             This says nothing about any bucket; run without --local for that."
        ),
        None => {
            println!("\nPASS — the bundle round-tripped byte-identically against a real bucket.")
        }
    }

    report_artifact(&report, &location, &bundle_path);
    Ok(())
}

/// The live-credential path. Split out so the local path above cannot silently
/// inherit a half-configured bucket.
async fn run_against_bucket(
    key: &ObjectKey,
    bundle: &[u8],
    cleanup: Cleanup,
) -> anyhow::Result<(RoundTripReport, String)> {
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

    let report = run_acceptance(&store, key, bundle, cleanup).await?;
    let location = format!("{}/{key}", config.bucket);
    Ok((report, location))
}

/// Print what an operator needs to check the claim from outside this tool.
fn report_artifact(report: &RoundTripReport, location: &str, bundle_path: &str) {
    if !report.kept {
        return;
    }
    println!("\n--- artifact left for independent inspection (--keep) ---");
    println!("  location:     {location}");
    println!("  key:          {}", report.key);
    println!("  size:         {} bytes", report.size);
    println!("  local sha256: {}", report.local_sha256);
    match &report.etag {
        Some(etag) => println!("  server etag:  {etag}"),
        None => println!("  server etag:  <none returned>"),
    }
    println!();
    println!("  Verify without trusting this tool:");
    println!("    1. Find {} in the store's object listing.", report.key);
    println!("    2. Download it and run:  sha256sum <downloaded>");
    println!("    3. It must equal the local sha256 above, and match:");
    println!("         sha256sum {bundle_path}");
    println!();
    println!("  The etag is advisory — S3-compatible stores differ on whether it is an");
    println!("  MD5, and multipart uploads change its shape. The sha256 comparison is");
    println!("  the authoritative check.");
    println!();
    println!("  Remember to delete the object when done; --keep left it deliberately.");
}
