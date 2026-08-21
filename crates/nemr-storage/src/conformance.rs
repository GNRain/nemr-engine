//! The conformance suite every [`ObjectStore`] must pass.
//!
//! Extracted from the bucket round-trip binary so the sequence is **executed in
//! tests against [`crate::local::LocalStore`]** rather than only against a live
//! bucket. Handing an operator a script whose logic has never run is how a
//! "failed acceptance" turns out to be a bug in the acceptance.
//!
//! It is also the honest way to add a backend: a new implementation is finished
//! when it passes this, not when its own hand-written expectations pass.

use crate::{ObjectKey, ObjectStore};

/// Exercise every trait method against `store`, leaving nothing behind.
///
/// `key` must not already exist: the suite asserts that first, so "it came
/// back" means this run put it there.
pub async fn round_trip(
    store: &impl ObjectStore,
    key: &ObjectKey,
    bundle: &[u8],
) -> anyhow::Result<()> {
    // 1. The object must not already exist, so "it came back" means this run put
    //    it there rather than a previous one having left it.
    print!("control: key is absent before … ");
    match store.head(key).await {
        Err(crate::StorageError::NotFound { .. }) => println!("ok"),
        Ok(_) => anyhow::bail!("the key already exists; a round trip would prove nothing"),
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

    // 7. Delete must be idempotent, so a retried cleanup does not fail.
    print!("delete idempotence … ");
    store.delete(key).await?;
    store.delete(key).await?;
    match store.head(key).await {
        Err(crate::StorageError::NotFound { .. }) => println!("ok (gone, second delete fine)"),
        Ok(_) => anyhow::bail!("the object survived deletion"),
        Err(e) => anyhow::bail!("unexpected error after deletion: {e}"),
    }

    Ok(())
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

        round_trip(&store, &key, &bundle)
            .await
            .expect("the local backend must pass the same suite a cloud backend must");
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

        let error = round_trip(&store, &key, b"the original content")
            .await
            .expect_err("a backend that corrupts content must FAIL conformance");
        assert!(
            error.to_string().contains("round-trip"),
            "the failure must name the round trip: {error}"
        );
    }
}
