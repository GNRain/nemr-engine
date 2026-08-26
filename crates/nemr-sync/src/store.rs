//! An object-safe façade over the M12 [`ObjectStore`] trait.
//!
//! `ObjectStore` uses native `async fn` in a trait, which is not
//! dyn-compatible, and a generic `T: ObjectStore` gives no bound that its
//! futures are `Send` — which `Arc<dyn DynStore>` behind an axum handler
//! requires. So each backend is adapted concretely: the compiler can prove a
//! *concrete* backend's futures are `Send`. Adding R2/B2 later is one more
//! forwarding impl via [`impl_dyn_store!`] — no vendor-specific behaviour
//! appears here, only delegation (E-11 / D-05).

use async_trait::async_trait;
use nemr_storage::local::LocalStore;
use nemr_storage::{ObjectKey, ObjectStore, Result};

#[async_trait]
pub trait DynStore: Send + Sync {
    fn describe(&self) -> String;
    async fn get(&self, key: &ObjectKey) -> Result<Vec<u8>>;
    async fn put(&self, key: &ObjectKey, bytes: &[u8]) -> Result<()>;
    async fn delete(&self, key: &ObjectKey) -> Result<()>;
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
            async fn delete(&self, key: &ObjectKey) -> Result<()> {
                ObjectStore::delete(self, key).await
            }
        }
    };
}

impl_dyn_store!(LocalStore);
// When R2/B2 land: `impl_dyn_store!(nemr_storage::s3::S3Store);` — config, not code.
