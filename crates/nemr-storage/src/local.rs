//! A filesystem-backed [`ObjectStore`].
//!
//! Two purposes, both real:
//!
//! 1. **The conformance target.** Every behaviour the trait promises — range
//!    clamping, idempotent delete, `NotFound` rather than a panic, sorted
//!    listing — is exercised here without a network or a credential, so a cloud
//!    backend can be checked against the same suite rather than against its own
//!    hand-written expectations.
//! 2. **A supported deployment.** E-11's test is whether someone can use the
//!    open half productively without ever paying, and the answer given was that
//!    a self-hoster moves bundles between their own machines with rsync. This is
//!    that path with an interface in front of it.
//!
//! Keys are validated by [`ObjectKey`] before they arrive, so a key can never
//! escape `root` — but this joins them component-wise anyway, because a store
//! that is one refactor away from a traversal is not a safe conformance target.

use std::path::{Path, PathBuf};

use crate::{ObjectKey, ObjectMeta, ObjectStore, Result, StorageError};

/// An object store backed by a directory.
#[derive(Debug, Clone)]
pub struct LocalStore {
    root: PathBuf,
}

impl LocalStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Resolve a key beneath `root`, refusing anything that leaves it.
    ///
    /// `ObjectKey` already rejects `..`, absolute keys and empty segments, so
    /// this is defence in depth — and it is the reason the local store is safe
    /// to point at a directory that contains anything else.
    fn path_for(&self, key: &ObjectKey) -> Result<PathBuf> {
        let mut path = self.root.clone();
        for segment in key.as_str().split('/') {
            if segment.is_empty() || segment == "." || segment == ".." {
                return Err(StorageError::InvalidKey {
                    key: key.as_str().to_string(),
                    reason: "segment would escape the store root".into(),
                });
            }
            path.push(segment);
        }
        Ok(path)
    }

    fn map_io(operation: &'static str, key: &ObjectKey, error: std::io::Error) -> StorageError {
        match error.kind() {
            std::io::ErrorKind::NotFound => StorageError::NotFound { key: key.clone() },
            std::io::ErrorKind::PermissionDenied => StorageError::AccessDenied {
                key: key.clone(),
                detail: error.to_string(),
            },
            _ => StorageError::Transient {
                operation,
                key: key.clone(),
                detail: error.to_string(),
            },
        }
    }
}

impl ObjectStore for LocalStore {
    fn describe(&self) -> String {
        format!("local:{}", self.root.display())
    }

    async fn head(&self, key: &ObjectKey) -> Result<ObjectMeta> {
        let path = self.path_for(key)?;
        let metadata = tokio::fs::metadata(&path)
            .await
            .map_err(|e| Self::map_io("head", key, e))?;
        Ok(ObjectMeta {
            key: key.clone(),
            size: metadata.len(),
            // No etag: a filesystem has none, and inventing one from mtime would
            // be a value callers could accidentally trust.
            etag: None,
        })
    }

    async fn get(&self, key: &ObjectKey) -> Result<Vec<u8>> {
        let path = self.path_for(key)?;
        tokio::fs::read(&path)
            .await
            .map_err(|e| Self::map_io("get", key, e))
    }

    async fn get_range(&self, key: &ObjectKey, range: std::ops::Range<u64>) -> Result<Vec<u8>> {
        use tokio::io::{AsyncReadExt, AsyncSeekExt};

        let path = self.path_for(key)?;
        let mut file = tokio::fs::File::open(&path)
            .await
            .map_err(|e| Self::map_io("get_range", key, e))?;
        let size = file
            .metadata()
            .await
            .map_err(|e| Self::map_io("get_range", key, e))?
            .len();

        // Clamp rather than error: the trait promises a caller may ask for a
        // manifest-sized prefix without a `head` first, which costs a round trip
        // and, on a metered backend, money.
        let start = range.start.min(size);
        let end = range.end.min(size);
        if start >= end {
            return Ok(Vec::new());
        }

        file.seek(std::io::SeekFrom::Start(start))
            .await
            .map_err(|e| Self::map_io("get_range", key, e))?;
        let mut buffer = vec![0u8; (end - start) as usize];
        file.read_exact(&mut buffer)
            .await
            .map_err(|e| Self::map_io("get_range", key, e))?;
        Ok(buffer)
    }

    async fn put(&self, key: &ObjectKey, bytes: &[u8]) -> Result<()> {
        let path = self.path_for(key)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| Self::map_io("put", key, e))?;
        }

        // Write to a temporary and rename, so a reader never observes a
        // half-written object. An interrupted upload leaving a truncated bundle
        // that passes a size check and fails a digest check is exactly the
        // failure M11 hardens against; there is no reason to create it here.
        let temporary = path.with_extension("partial");
        tokio::fs::write(&temporary, bytes)
            .await
            .map_err(|e| Self::map_io("put", key, e))?;
        tokio::fs::rename(&temporary, &path)
            .await
            .map_err(|e| Self::map_io("put", key, e))?;
        Ok(())
    }

    async fn delete(&self, key: &ObjectKey) -> Result<()> {
        let path = self.path_for(key)?;
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            // Absent is success: deletion is idempotent so a retried cleanup
            // after a partial failure does not fail.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Self::map_io("delete", key, e)),
        }
    }

    async fn list(&self, prefix: &str) -> Result<Vec<ObjectKey>> {
        let mut found = Vec::new();
        let mut stack = vec![self.root.clone()];
        while let Some(directory) = stack.pop() {
            let mut entries = match tokio::fs::read_dir(&directory).await {
                Ok(entries) => entries,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    return Err(StorageError::Other(
                        anyhow::Error::from(e).context(format!("listing {}", directory.display())),
                    ))
                }
            };
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().is_some_and(|e| e == "partial") {
                    continue; // an in-flight put is not an object yet
                }
                if let Some(key) = relative_key(&self.root, &path) {
                    if key.starts_with(prefix) {
                        if let Ok(key) = ObjectKey::new(key) {
                            found.push(key);
                        }
                    }
                }
            }
        }
        found.sort();
        Ok(found)
    }
}

fn relative_key(root: &Path, path: &Path) -> Option<String> {
    Some(
        path.strip_prefix(root)
            .ok()?
            .to_string_lossy()
            .replace('\\', "/"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (LocalStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        (LocalStore::new(dir.path()), dir)
    }

    fn key(k: &str) -> ObjectKey {
        ObjectKey::new(k).expect("valid key")
    }

    #[tokio::test]
    async fn put_get_round_trips() {
        let (store, _dir) = store();
        let k = key("projects/demo/bundle.nemr");
        store.put(&k, b"payload").await.unwrap();
        assert_eq!(store.get(&k).await.unwrap(), b"payload");
    }

    #[tokio::test]
    async fn head_reports_size_without_transferring() {
        let (store, _dir) = store();
        let k = key("a.bin");
        store.put(&k, &vec![7u8; 4096]).await.unwrap();
        let meta = store.head(&k).await.unwrap();
        assert_eq!(meta.size, 4096);
        assert_eq!(meta.key, k);
    }

    /// The egress-conscious path: read a bundle's manifest prefix without
    /// pulling the whole object.
    #[tokio::test]
    async fn get_range_returns_a_prefix_and_clamps_past_the_end() {
        let (store, _dir) = store();
        let k = key("bundle.nemr");
        store.put(&k, b"0123456789").await.unwrap();

        assert_eq!(store.get_range(&k, 0..4).await.unwrap(), b"0123");
        assert_eq!(store.get_range(&k, 4..8).await.unwrap(), b"4567");
        // Clamped, not an error: a caller may ask for a manifest-sized prefix
        // without a head() first.
        assert_eq!(store.get_range(&k, 8..9_999).await.unwrap(), b"89");
        assert_eq!(store.get_range(&k, 50..60).await.unwrap(), b"");
    }

    #[tokio::test]
    async fn missing_objects_report_not_found_rather_than_a_transient() {
        let (store, _dir) = store();
        let error = store.get(&key("absent")).await.unwrap_err();
        assert!(
            matches!(error, StorageError::NotFound { .. }),
            "got {error:?}"
        );
        assert!(
            !error.is_retryable(),
            "a missing object must not be retried — it burns egress on a certainty"
        );
    }

    #[tokio::test]
    async fn delete_is_idempotent() {
        let (store, _dir) = store();
        let k = key("gone.bin");
        store.put(&k, b"x").await.unwrap();
        store.delete(&k).await.unwrap();
        // Second delete must succeed, so a retried cleanup after a partial
        // failure does not fail.
        store.delete(&k).await.unwrap();
        assert!(matches!(
            store.get(&k).await.unwrap_err(),
            StorageError::NotFound { .. }
        ));
    }

    #[tokio::test]
    async fn list_is_sorted_and_prefix_filtered() {
        let (store, _dir) = store();
        for k in ["projects/b.nemr", "projects/a.nemr", "other/c.nemr"] {
            store.put(&key(k), b"x").await.unwrap();
        }
        let listed: Vec<String> = store
            .list("projects/")
            .await
            .unwrap()
            .iter()
            .map(|k| k.to_string())
            .collect();
        assert_eq!(listed, vec!["projects/a.nemr", "projects/b.nemr"]);
    }

    /// A `put` must be atomic: a reader either sees the previous object or the
    /// new one, never a partial write. The temporary file must not be listed.
    #[tokio::test]
    async fn an_in_flight_put_is_not_listed_as_an_object() {
        let (store, dir) = store();
        store.put(&key("real.bin"), b"x").await.unwrap();
        // Simulate a crashed upload leaving its temporary behind.
        std::fs::write(dir.path().join("stale.partial"), b"half").unwrap();

        let listed: Vec<String> = store
            .list("")
            .await
            .unwrap()
            .iter()
            .map(|k| k.to_string())
            .collect();
        assert_eq!(
            listed,
            vec!["real.bin"],
            "a leftover .partial is not an object and must not be listed"
        );
    }
}
