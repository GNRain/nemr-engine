//! An object-safe façade over the M12 [`ObjectStore`] trait.
//!
//! `ObjectStore` uses native `async fn` in a trait, which is not
//! dyn-compatible, and a generic `T: ObjectStore` gives no bound that its
//! futures are `Send` — which `Arc<dyn DynStore>` behind an axum handler
//! requires. So each backend is adapted concretely: the compiler can prove a
//! *concrete* backend's futures are `Send`. Adding a backend is one more
//! forwarding impl via [`impl_dyn_store!`] — no vendor-specific behaviour
//! appears here, only delegation (E-11 / D-05).
//!
//! E-20 (ruled 2026-09-07): the backend is chosen from the settings by
//! [`open_store`] — exactly one of a directory and an object store, no
//! default — and [`preflight`] must succeed before the database is migrated
//! and before the port binds, so a server that is listening is one whose
//! store answered.

use async_trait::async_trait;
use nemr_storage::local::LocalStore;
use nemr_storage::s3::{S3Config, S3Store};
use nemr_storage::{ObjectKey, ObjectStore, Result};

use crate::settings::StorageChoice;

#[async_trait]
pub trait DynStore: Send + Sync {
    fn describe(&self) -> String;
    async fn get(&self, key: &ObjectKey) -> Result<Vec<u8>>;
    async fn put(&self, key: &ObjectKey, bytes: &[u8]) -> Result<()>;
    /// F-16: store an object by streaming it from a staged file.
    ///
    /// Defaulted, like [`ObjectStore::put_file`] and for the same reason: the
    /// real backends override it through the forwarding macro below, and a test
    /// double that only needs `list` to fail should not have to implement the
    /// streaming paths — a required method here breaks every double in the tree
    /// the day it is added.
    async fn put_file(&self, key: &ObjectKey, path: &std::path::Path) -> Result<()> {
        let bytes = tokio::fs::read(path).await.map_err(|e| {
            nemr_storage::StorageError::Other(
                anyhow::Error::from(e).context("reading the staged object"),
            )
        })?;
        self.put(key, &bytes).await
    }

    /// F-16: read an object as a stream of chunks, with its size, so answering
    /// a download never buffers a multi-gigabyte bundle in the server.
    async fn get_stream(&self, key: &ObjectKey) -> Result<(u64, nemr_storage::ByteStream)> {
        let bytes = self.get(key).await?;
        let size = bytes.len() as u64;
        let once = futures::stream::once(async move { Ok(bytes::Bytes::from(bytes)) });
        Ok((size, Box::pin(once)))
    }
    async fn delete(&self, key: &ObjectKey) -> Result<()>;
    /// Keys under a prefix. The start-up probe lists a prefix no bundle key
    /// can match: a reachable store answers with an empty page, an
    /// unreachable one with the error the operator needs to see.
    async fn list(&self, prefix: &str) -> Result<Vec<ObjectKey>>;
}

/// Forward every `DynStore` method to the backend's `ObjectStore` impl. The body
/// is pure delegation; a backend that needed anything else here would be
/// vendor-specific behaviour above the trait, which E-11 forbids.
macro_rules! impl_dyn_store {
    ($backend:ty) => {
        #[async_trait]
        impl DynStore for $backend {
            fn describe(&self) -> String {
                ObjectStore::describe(self)
            }
            async fn get(&self, key: &ObjectKey) -> Result<Vec<u8>> {
                ObjectStore::get(self, key).await
            }
            async fn put(&self, key: &ObjectKey, bytes: &[u8]) -> Result<()> {
                ObjectStore::put(self, key, bytes).await
            }
            async fn put_file(&self, key: &ObjectKey, path: &std::path::Path) -> Result<()> {
                ObjectStore::put_file(self, key, path).await
            }
            async fn get_stream(&self, key: &ObjectKey) -> Result<(u64, nemr_storage::ByteStream)> {
                ObjectStore::get_stream(self, key).await
            }
            async fn delete(&self, key: &ObjectKey) -> Result<()> {
                ObjectStore::delete(self, key).await
            }
            async fn list(&self, prefix: &str) -> Result<Vec<ObjectKey>> {
                ObjectStore::list(self, prefix).await
            }
        }
    };
}

impl_dyn_store!(LocalStore);
impl_dyn_store!(S3Store);

/// Open the chosen backend. Construction makes no network call; the
/// probe is [`preflight`].
pub fn open_store(
    choice: &StorageChoice,
    get: &dyn Fn(&str) -> Option<String>,
) -> anyhow::Result<std::sync::Arc<dyn DynStore>> {
    Ok(match choice {
        StorageChoice::Local(dir) => std::sync::Arc::new(LocalStore::new(dir.clone())),
        StorageChoice::S3 => {
            let config = S3Config::from_lookup(get)
                .map_err(|e| anyhow::anyhow!("{e}"))?
                .ok_or_else(|| anyhow::anyhow!("NEMR_S3_BUCKET is not set"))?;
            std::sync::Arc::new(S3Store::new(&config).map_err(|e| anyhow::anyhow!("{e}"))?)
        }
    })
}

/// The prefix the probe lists: under the bundle prefix, in a segment no
/// `{prefix}/{user_uuid}/{session_uuid}` key can match.
pub fn probe_prefix(bundle_prefix: &str) -> String {
    format!("{bundle_prefix}/.startup-probe")
}

/// One egress-free list against the store. A failure here is the store's
/// own message, with any query string cut off by the backend (a
/// credential never reaches this text).
pub async fn preflight(store: &dyn DynStore, bundle_prefix: &str) -> anyhow::Result<()> {
    store
        .list(&probe_prefix(bundle_prefix))
        .await
        .map(|_| ())
        .map_err(|e| {
            anyhow::anyhow!(
                "the storage backend ({}) did not answer: {e}",
                store.describe()
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use nemr_storage::StorageError;

    /// A store whose list fails: what a wrong bucket, a revoked key or an
    /// unreachable endpoint look like from here.
    struct FailsOnList;
    #[async_trait]
    impl DynStore for FailsOnList {
        fn describe(&self) -> String {
            "fails:on-list".into()
        }
        async fn get(&self, _: &ObjectKey) -> Result<Vec<u8>> {
            unreachable!()
        }
        async fn put(&self, _: &ObjectKey, _: &[u8]) -> Result<()> {
            unreachable!()
        }
        async fn delete(&self, _: &ObjectKey) -> Result<()> {
            unreachable!()
        }
        async fn list(&self, _: &str) -> Result<Vec<ObjectKey>> {
            Err(StorageError::Other(anyhow::anyhow!(
                "AccessDenied: the credential is not accepted?X-Amz-Credential=should-not-appear"
            )))
        }
    }

    /// The preflight refuses a store that cannot list, naming the store;
    /// the control is a directory store, which answers. The refusal never
    /// carries a query string.
    #[tokio::test]
    async fn preflight_refuses_a_store_that_cannot_list_and_passes_one_that_can() {
        let err = preflight(&FailsOnList, "bundles")
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("fails:on-list") && err.contains("did not answer"),
            "{err}"
        );
        let dir = tempfile::tempdir().unwrap();
        let ok = LocalStore::new(dir.path().to_path_buf());
        preflight(&ok, "bundles")
            .await
            .expect("a directory store answers");
    }

    /// The probe's prefix can never match a bundle key.
    #[test]
    fn the_probe_prefix_is_outside_every_bundle_key() {
        let p = probe_prefix("bundles");
        let key = format!("bundles/{}/{}", uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        assert!(!key.starts_with(&p));
    }

    /// The object store is a `DynStore` at compile time — the line the
    /// facade reserved for it.
    #[test]
    fn the_s3_backend_is_a_dyn_store() {
        fn takes(_: &dyn DynStore) {}
        let cfg = S3Config::from_lookup(&|k: &str| match k {
            "NEMR_S3_PROVIDER" => Some("r2".into()),
            "NEMR_S3_BUCKET" => Some("b".into()),
            "NEMR_S3_ENDPOINT" => Some("https://example.invalid".into()),
            "NEMR_S3_ACCESS_KEY_ID" => Some("k".into()),
            "NEMR_S3_SECRET_ACCESS_KEY" => Some("s".into()),
            _ => None,
        })
        .unwrap()
        .unwrap();
        let store = S3Store::new(&cfg).unwrap();
        takes(&store);
    }
}
