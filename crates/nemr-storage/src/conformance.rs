//! The conformance suite every [`ObjectStore`] must pass.
//!
//! Extracted from the bucket round-trip binary so the sequence is **executed in
//! tests against [`crate::local::LocalStore`]** rather than only against a live
//! bucket. Handing an operator a script whose logic has never run is how a
//! "failed acceptance" turns out to be a bug in the acceptance.
//!
//! It is also the honest way to add a backend: a new implementation is finished
//! when it passes this, not when its own hand-written expectations pass.

use sha2::{Digest, Sha256};

use crate::{ObjectKey, ObjectStore};

/// Whether the suite removes the object it wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cleanup {
    /// Delete the object, and exercise delete-idempotence while doing so.
    Remove,
    /// Leave the object in place for independent inspection (F-61).
    ///
    /// The delete-idempotence step is skipped, which the suite states in its
    /// output rather than leaving the operator to infer: a run that keeps its
    /// artifact verifies strictly less than one that cleans up, and saying so is
    /// the difference between a weaker claim and a misleading one.
    Keep,
}

/// What the round trip observed, so a caller can report facts an operator can
/// check independently rather than only a summary they must trust (F-61).
#[derive(Debug, Clone)]
pub struct RoundTripReport {
    pub key: ObjectKey,
    pub size: u64,
    /// SHA-256 computed locally over the bytes that were uploaded.
    pub local_sha256: String,
    /// Entity tag the server returned for the stored object, if any.
    ///
    /// Advisory and **not** an integrity check: S3-compatible stores differ on
    /// whether this is an MD5 of the content, and a multipart upload changes its
    /// shape entirely. It is printed so an operator can compare it against what
    /// the provider's console shows for the same object — a cross-check of the
    /// tool's claim against a source the tool does not control.
    pub etag: Option<String>,
    pub kept: bool,
}

/// The pre-flight control failed: something was already at `key`.
///
/// Distinguished from every other failure because it is the one case where the
/// run wrote nothing, so cleanup must NOT delete — the object belongs to
/// whoever put it there. Deleting another party's object while reporting a
/// clean failure is the VOL-05 shape: the wrong action under a truthful-looking
/// summary.
#[derive(Debug, thiserror::Error)]
#[error("{key} already exists; a round trip would prove nothing (not deleting it — this run did not create it)")]
pub struct PreexistingKey {
    pub key: ObjectKey,
}

/// Exercise every trait method against `store`.
///
/// `key` must not already exist: the suite asserts that first, so "it came
/// back" means this run put it there.
pub async fn round_trip(
    store: &impl ObjectStore,
    key: &ObjectKey,
    bundle: &[u8],
    cleanup: Cleanup,
) -> anyhow::Result<RoundTripReport> {
    // 1. The object must not already exist, so "it came back" means this run put
    //    it there rather than a previous one having left it.
    print!("control: key is absent before … ");
    match store.head(key).await {
        Err(crate::StorageError::NotFound { .. }) => println!("ok"),
        Ok(_) => return Err(PreexistingKey { key: key.clone() }.into()),
        Err(e) => anyhow::bail!("unexpected error checking the key: {e}"),
    }

    print!("put … ");
    store.put(key, bundle).await?;
    println!("ok ({} bytes)", bundle.len());

    // 2. head must report the right size WITHOUT transferring (D-05).
    print!("head (no egress) … ");
    let meta = store.head(key).await?;
    anyhow::ensure!(
        meta.size == bundle.len() as u64,
        "head reported {} bytes, expected {}",
        meta.size,
        bundle.len()
    );
    println!("ok (size {} matches)", meta.size);
    let etag = meta.etag.clone();

    // 3. get_range must return the manifest prefix — the egress-conscious path
    //    that makes "what is in this bundle?" cheap.
    print!("get_range (manifest prefix) … ");
    let prefix_len = 512.min(bundle.len() as u64);
    let prefix = store.get_range(key, 0..prefix_len).await?;
    anyhow::ensure!(
        prefix == bundle[..prefix_len as usize],
        "the range did not match the bundle's first {prefix_len} bytes"
    );
    println!("ok ({prefix_len} bytes, matches)");

    // 4. A range past the end must clamp rather than error, per the trait.
    print!("get_range (clamped past end) … ");
    let clamped = store.get_range(key, bundle.len() as u64..bundle.len() as u64 + 4096).await?;
    anyhow::ensure!(clamped.is_empty(), "a range past the end must clamp to empty");
    println!("ok");

    // 5. The whole object must come back byte-identical. This is the acceptance.
    print!("get (full) … ");
    let fetched = store.get(key).await?;
    anyhow::ensure!(
        fetched == bundle,
        "the bundle did NOT round-trip: {} bytes out, {} back",
        bundle.len(),
        fetched.len()
    );
    println!("ok ({} bytes, byte-identical)", fetched.len());

    // 6. It must be listed under its prefix.
    print!("list … ");
    let listed = store.list("nemr-roundtrip/").await?;
    anyhow::ensure!(
        listed.contains(key),
        "the object was not listed under its prefix (found {} keys)",
        listed.len()
    );
    println!("ok (found among {} key(s))", listed.len());

    // 7. Delete must be idempotent, so a retried cleanup does not fail — unless
    //    the caller asked to keep the artifact for inspection.
    match cleanup {
        Cleanup::Remove => {
            print!("delete idempotence … ");
            store.delete(key).await?;
            store.delete(key).await?;
            match store.head(key).await {
                Err(crate::StorageError::NotFound { .. }) => {
                    println!("ok (gone, second delete fine)")
                }
                Ok(_) => anyhow::bail!("the object survived deletion"),
                Err(e) => anyhow::bail!("unexpected error after deletion: {e}"),
            }
        }
        Cleanup::Keep => {
            println!("delete idempotence … SKIPPED (--keep): the object is left in place");
        }
    }

    Ok(RoundTripReport {
        key: key.clone(),
        size: bundle.len() as u64,
        local_sha256: hex(&Sha256::digest(bundle)),
        etag,
        kept: cleanup == Cleanup::Keep,
    })
}

/// The full acceptance: run the suite, then honour `cleanup` **whatever the
/// outcome**.
///
/// The cleanup decision lives here rather than in the binary deliberately. It
/// has two properties that are easy to get wrong and impossible to check by
/// reading a summary:
///
///   * a **failed** default run must still delete what it wrote, so a scoped
///     credential is never left holding an object the operator must hunt for;
///   * a **failed** `--keep` run must still leave the object, because a failed
///     acceptance is precisely when someone needs the artifact to look at.
///
/// A cleanup path that only runs on success is the ordinary version of this
/// bug, and it only shows up on the day something fails. Both properties are
/// asserted in this module's tests against a backend with an injected failure.
pub async fn run_acceptance(
    store: &impl ObjectStore,
    key: &ObjectKey,
    bundle: &[u8],
    cleanup: Cleanup,
) -> anyhow::Result<RoundTripReport> {
    let outcome = round_trip(store, key, bundle, cleanup).await;

    // Each step prints its label before running and "ok" after, so a step that
    // fails leaves a dangling "list … " that the next line would complete —
    // making a failed step read as a passed one in pasted output. Close it.
    if let Err(e) = &outcome {
        println!("FAILED\n  {e}");
    }

    match cleanup {
        Cleanup::Keep => println!("cleanup … SKIPPED (--keep): the object is left in place"),
        // A pre-flight failure means this run never wrote the object, so there is
        // nothing of ours to remove and the object is someone else's.
        Cleanup::Remove
            if outcome
                .as_ref()
                .err()
                .is_some_and(|e| e.downcast_ref::<PreexistingKey>().is_some()) =>
        {
            println!("cleanup … SKIPPED: this run did not create {key}");
        }
        Cleanup::Remove => {
            print!("cleanup … ");
            match store.delete(key).await {
                Ok(()) => println!("deleted {key}"),
                // Reported, not propagated: a cleanup failure must not mask the
                // acceptance result, which is what the operator actually asked.
                Err(e) => println!("FAILED to delete {key}: {e}  (remove it manually)"),
            }
        }
    }

    outcome
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local::LocalStore;

    /// The conformance suite must pass against the local backend.
    ///
    /// This is what makes the bucket run trustworthy: the sequence an operator
    /// executes against a live credential has already been executed here. A
    /// "failed acceptance" is then a fact about the backend rather than possibly
    /// a bug in the acceptance.
    #[tokio::test]
    async fn the_local_backend_passes_conformance() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStore::new(dir.path());
        let key = ObjectKey::new("nemr-roundtrip/local.nemr").unwrap();

        // Realistic size: large enough that zstd-compressed bundle content is
        // not a trivial case, and that a range request is a real prefix.
        let bundle: Vec<u8> = (0..40_000u32).flat_map(|i| i.to_le_bytes()).collect();

        let report = run_acceptance(&store, &key, &bundle, Cleanup::Remove)
            .await
            .expect("the local backend must pass the same suite a cloud backend must");
        assert!(!report.kept);
        assert_eq!(report.size, bundle.len() as u64);
        assert_eq!(report.local_sha256, hex(&Sha256::digest(&bundle)));
    }

    /// `--keep` must leave the object behind, and say that it verified less.
    ///
    /// F-61: the default run deletes what it wrote, so an operator has nothing
    /// to inspect and must trust the tool's summary. `--keep` is what makes the
    /// acceptance falsifiable from outside the tool.
    #[tokio::test]
    async fn keep_leaves_the_object_for_independent_inspection() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStore::new(dir.path());
        let key = ObjectKey::new("nemr-roundtrip/kept.nemr").unwrap();
        let bundle = b"inspectable payload".to_vec();

        let report = run_acceptance(&store, &key, &bundle, Cleanup::Keep)
            .await
            .expect("the suite must pass with --keep");

        assert!(report.kept, "the report must say the artifact was kept");
        // The object is still there, and is byte-identical to what went in —
        // which is the property the operator will check by hand.
        assert_eq!(
            store.get(&key).await.expect("the kept object must still exist"),
            bundle,
            "the kept artifact must be byte-identical to the uploaded bundle"
        );
        assert_eq!(report.local_sha256, hex(&Sha256::digest(&bundle)));
    }

    /// A FAILING run must still honour `cleanup` — both directions.
    ///
    /// The failure is injected at `list`, i.e. *after* the object is stored, so
    /// the run aborts with an artifact in the store and the cleanup decision
    /// actually matters. A cleanup path that only runs on success passes every
    /// happy-path test and fails on the one day it counts.
    #[tokio::test]
    async fn a_failing_run_honours_the_cleanup_choice() {
        struct FailsOnList(LocalStore);

        impl ObjectStore for FailsOnList {
            fn describe(&self) -> String {
                "fails-on-list".into()
            }
            async fn head(&self, key: &ObjectKey) -> crate::Result<crate::ObjectMeta> {
                self.0.head(key).await
            }
            async fn get(&self, key: &ObjectKey) -> crate::Result<Vec<u8>> {
                self.0.get(key).await
            }
            async fn get_range(
                &self,
                key: &ObjectKey,
                range: std::ops::Range<u64>,
            ) -> crate::Result<Vec<u8>> {
                self.0.get_range(key, range).await
            }
            async fn put(&self, key: &ObjectKey, bytes: &[u8]) -> crate::Result<()> {
                self.0.put(key, bytes).await
            }
            async fn delete(&self, key: &ObjectKey) -> crate::Result<()> {
                self.0.delete(key).await
            }
            async fn list(&self, _prefix: &str) -> crate::Result<Vec<ObjectKey>> {
                Err(crate::StorageError::Other(anyhow::anyhow!(
                    "injected list failure"
                )))
            }
        }

        // --keep: the failed run must LEAVE the object.
        let kept_dir = tempfile::tempdir().unwrap();
        let kept = FailsOnList(LocalStore::new(kept_dir.path()));
        let key = ObjectKey::new("nemr-roundtrip/failed.nemr").unwrap();
        run_acceptance(&kept, &key, b"payload", Cleanup::Keep)
            .await
            .expect_err("the injected failure must fail the run");
        assert_eq!(
            kept.get(&key)
                .await
                .expect("a failed --keep run must still leave the object"),
            b"payload",
            "the artifact must survive a failed run, which is when it is most needed"
        );

        // default: the failed run must still CLEAN UP, so a scoped credential is
        // not left holding an object after an aborted acceptance.
        let swept_dir = tempfile::tempdir().unwrap();
        let swept = FailsOnList(LocalStore::new(swept_dir.path()));
        run_acceptance(&swept, &key, b"payload", Cleanup::Remove)
            .await
            .expect_err("the injected failure must fail the run");
        assert!(
            matches!(
                swept.head(&key).await,
                Err(crate::StorageError::NotFound { .. })
            ),
            "a failed default run must still delete what it wrote"
        );
    }

    /// A pre-existing object must survive a failed run, even by default.
    ///
    /// Cleanup deletes what the run wrote. If the pre-flight control trips, the
    /// run wrote nothing — so deleting would destroy an object belonging to
    /// whoever put it there, while the summary reads like an ordinary tidy-up.
    #[tokio::test]
    async fn cleanup_never_deletes_an_object_this_run_did_not_write() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStore::new(dir.path());
        let key = ObjectKey::new("nemr-roundtrip/someone-elses.nemr").unwrap();
        store.put(&key, b"not ours").await.unwrap();

        let error = run_acceptance(&store, &key, b"ours", Cleanup::Remove)
            .await
            .expect_err("a pre-existing key must fail the control");
        assert!(
            error.downcast_ref::<PreexistingKey>().is_some(),
            "the pre-flight failure must stay distinguishable, got: {error}"
        );
        assert_eq!(
            store.get(&key).await.expect("the foreign object must survive"),
            b"not ours",
            "cleanup must not delete an object this run did not create"
        );
    }

    /// The suite must FAIL against a backend that does not round-trip.
    ///
    /// Without this, a suite that silently accepted anything would still report
    /// PASS against a real bucket — the acceptance would be theatre. Proven by a
    /// deliberately broken store whose `get` returns the wrong bytes.
    #[tokio::test]
    async fn the_suite_rejects_a_backend_that_corrupts_content() {
        struct Corrupting(LocalStore);

        impl ObjectStore for Corrupting {
            fn describe(&self) -> String {
                "corrupting".into()
            }
            async fn head(&self, key: &ObjectKey) -> crate::Result<crate::ObjectMeta> {
                self.0.head(key).await
            }
            async fn get(&self, key: &ObjectKey) -> crate::Result<Vec<u8>> {
                // One flipped byte: the failure a digest check exists to catch.
                let mut bytes = self.0.get(key).await?;
                if let Some(first) = bytes.first_mut() {
                    *first ^= 0xFF;
                }
                Ok(bytes)
            }
            async fn get_range(
                &self,
                key: &ObjectKey,
                range: std::ops::Range<u64>,
            ) -> crate::Result<Vec<u8>> {
                self.0.get_range(key, range).await
            }
            async fn put(&self, key: &ObjectKey, bytes: &[u8]) -> crate::Result<()> {
                self.0.put(key, bytes).await
            }
            async fn delete(&self, key: &ObjectKey) -> crate::Result<()> {
                self.0.delete(key).await
            }
            async fn list(&self, prefix: &str) -> crate::Result<Vec<ObjectKey>> {
                self.0.list(prefix).await
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let store = Corrupting(LocalStore::new(dir.path()));
        let key = ObjectKey::new("nemr-roundtrip/corrupt.nemr").unwrap();

        let error = round_trip(&store, &key, b"the original content", Cleanup::Remove)
            .await
            .expect_err("a backend that corrupts content must FAIL conformance");
        assert!(
            error.to_string().contains("round-trip"),
            "the failure must name the round trip: {error}"
        );
    }
}
