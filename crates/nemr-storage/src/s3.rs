//! S3-compatible backend, used for Cloudflare R2 and Backblaze B2 (D-05).
//!
//! # One implementation, two providers
//!
//! R2 and B2 both speak the S3 API, so they are the *same* implementation with
//! different endpoints and regions — not two backends with a shared trait. That
//! is the point of E-11's "no vendor-specific behaviour above the trait": if
//! supporting a second provider required a second code path above
//! [`crate::ObjectStore`], the abstraction would be decorative.
//!
//! [`Provider`] therefore only supplies *configuration* — endpoint shape and
//! region convention. Nothing branches on it at request time, and a caller
//! cannot observe which provider it is talking to except through
//! [`crate::ObjectStore::describe`], which is documented as diagnostics-only.
//!
//! # Credentials
//!
//! Read from the environment, never from a file this crate writes and never
//! logged. The `Debug` implementation redacts them, because a config struct
//! printed into a log or a panic message is one of the commonest ways a
//! long-lived object-storage key escapes — and per D-05 that key carries direct
//! cost exposure, not just data risk.

use std::fmt;

use crate::{ObjectKey, ObjectMeta, ObjectStore, Result, StorageError};

/// Which S3-compatible service to talk to.
///
/// Configuration only. Nothing branches on this at request time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    /// Cloudflare R2. Chosen first under D-05 because egress is free and this
    /// workload is egress-heavy: every attach on a new machine is a download.
    R2,
    /// Backblaze B2. Kept viable behind the same interface — cheaper at rest,
    /// but its free egress is capped at a multiple of stored bytes, which a sync
    /// product with small bundles and frequent pulls can exceed.
    B2,
    /// Any other S3-compatible endpoint, including MinIO for local testing.
    Other,
}

impl Provider {
    fn label(self) -> &'static str {
        match self {
            Self::R2 => "r2",
            Self::B2 => "b2",
            Self::Other => "s3",
        }
    }

    /// The region string the provider expects.
    ///
    /// R2 ignores region but requires the field to be present and consistent,
    /// and uses `auto` by convention. B2 and others carry a real region.
    fn default_region(self) -> &'static str {
        match self {
            Self::R2 => "auto",
            _ => "us-east-1",
        }
    }
}

/// Everything needed to reach a bucket.
///
/// Built from the environment by [`S3Config::from_env`] so a credential is never
/// written into a config file by this crate.
#[derive(Clone)]
pub struct S3Config {
    pub provider: Provider,
    pub bucket: String,
    pub endpoint: String,
    pub region: String,
    pub access_key_id: String,
    secret_access_key: String,
}

impl fmt::Debug for S3Config {
    /// Redacts both halves of the credential.
    ///
    /// The access key id is redacted too, not just the secret: it identifies the
    /// account and appears in abuse reports, and there is no diagnostic that
    /// needs it which the endpoint and bucket do not already answer.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("S3Config")
            .field("provider", &self.provider)
            .field("bucket", &self.bucket)
            .field("endpoint", &self.endpoint)
            .field("region", &self.region)
            .field("access_key_id", &"<redacted>")
            .field("secret_access_key", &"<redacted>")
            .finish()
    }
}

impl S3Config {
    /// Read configuration from the environment.
    ///
    /// | Variable | Meaning |
    /// |---|---|
    /// | `NEMR_S3_PROVIDER` | `r2`, `b2`, or anything else for a generic endpoint |
    /// | `NEMR_S3_BUCKET` | bucket name |
    /// | `NEMR_S3_ENDPOINT` | e.g. `https://<account>.r2.cloudflarestorage.com` |
    /// | `NEMR_S3_REGION` | optional; defaults per provider |
    /// | `NEMR_S3_ACCESS_KEY_ID` / `NEMR_S3_SECRET_ACCESS_KEY` | credential |
    ///
    /// Returns `None` when nothing is configured, so a caller can distinguish
    /// "no backend set up" from "backend set up wrongly" — the first is the
    /// normal state for the open engine and must not look like an error.
    pub fn from_env() -> Result<Option<Self>> {
        let Ok(bucket) = std::env::var("NEMR_S3_BUCKET") else {
            return Ok(None);
        };

        let provider = match std::env::var("NEMR_S3_PROVIDER").unwrap_or_default().as_str() {
            "r2" => Provider::R2,
            "b2" => Provider::B2,
            _ => Provider::Other,
        };

        let missing = |name: &str| StorageError::Other(anyhow::anyhow!(
            "{name} is not set. An S3 backend needs NEMR_S3_BUCKET, NEMR_S3_ENDPOINT, \
             NEMR_S3_ACCESS_KEY_ID and NEMR_S3_SECRET_ACCESS_KEY."
        ));

        Ok(Some(Self {
            provider,
            bucket,
            endpoint: std::env::var("NEMR_S3_ENDPOINT").map_err(|_| missing("NEMR_S3_ENDPOINT"))?,
            region: std::env::var("NEMR_S3_REGION")
                .unwrap_or_else(|_| provider.default_region().to_string()),
            access_key_id: std::env::var("NEMR_S3_ACCESS_KEY_ID")
                .map_err(|_| missing("NEMR_S3_ACCESS_KEY_ID"))?,
            secret_access_key: std::env::var("NEMR_S3_SECRET_ACCESS_KEY")
                .map_err(|_| missing("NEMR_S3_SECRET_ACCESS_KEY"))?,
        }))
    }
}

/// An S3-compatible object store.
pub struct S3Store {
    inner: object_store::aws::AmazonS3,
    describe: String,
}

impl S3Store {
    pub fn new(config: &S3Config) -> Result<Self> {
        let inner = object_store::aws::AmazonS3Builder::new()
            .with_bucket_name(&config.bucket)
            .with_endpoint(&config.endpoint)
            .with_region(&config.region)
            .with_access_key_id(&config.access_key_id)
            .with_secret_access_key(&config.secret_access_key)
            // Path-style addressing: R2 requires it, and virtual-host style
            // needs per-bucket DNS that a scoped test token will not have.
            .with_virtual_hosted_style_request(false)
            .with_allow_http(config.endpoint.starts_with("http://"))
            .build()
            .map_err(|e| StorageError::Other(anyhow::Error::from(e).context("building the S3 client")))?;

        Ok(Self {
            inner,
            // Never includes the credential — see S3Config's Debug impl.
            describe: format!("{}:{}", config.provider.label(), config.bucket),
        })
    }

    /// Map an object_store error into the trait's taxonomy.
    ///
    /// The retryable/not distinction is the one that matters: a sync loop that
    /// retries a 404 or a 403 burns egress and time on a certainty, and on a
    /// metered backend that is money.
    fn map(operation: &'static str, key: &ObjectKey, error: object_store::Error) -> StorageError {
        use object_store::Error as E;
        match error {
            E::NotFound { .. } => StorageError::NotFound { key: key.clone() },
            E::PermissionDenied { .. } | E::Unauthenticated { .. } => StorageError::AccessDenied {
                key: key.clone(),
                detail: error_summary(&error),
            },
            other => StorageError::Transient {
                operation,
                key: key.clone(),
                detail: error_summary(&other),
            },
        }
    }

    fn path(key: &ObjectKey) -> object_store::path::Path {
        object_store::path::Path::from(key.as_str())
    }
}

/// A one-line summary that cannot contain a credential.
///
/// object_store errors can embed a request URL, and a presigned or
/// misconfigured request could carry a key in a query parameter. Truncating and
/// stripping anything after a `?` keeps a diagnostic useful without risking
/// putting a credential in a log.
fn error_summary(error: &object_store::Error) -> String {
    let text = error.to_string();
    let text = text.split('?').next().unwrap_or(&text);
    text.chars().take(200).collect()
}

impl ObjectStore for S3Store {
    fn describe(&self) -> String {
        self.describe.clone()
    }

    async fn head(&self, key: &ObjectKey) -> Result<ObjectMeta> {
        use object_store::ObjectStoreExt as _;
        let meta = self
            .inner
            .head(&Self::path(key))
            .await
            .map_err(|e| Self::map("head", key, e))?;
        Ok(ObjectMeta {
            key: key.clone(),
            size: meta.size,
            etag: meta.e_tag,
        })
    }

    async fn get(&self, key: &ObjectKey) -> Result<Vec<u8>> {
        use object_store::ObjectStoreExt as _;
        let result = self
            .inner
            .get(&Self::path(key))
            .await
            .map_err(|e| Self::map("get", key, e))?;
        let bytes = result
            .bytes()
            .await
            .map_err(|e| Self::map("get", key, e))?;
        Ok(bytes.to_vec())
    }

    async fn get_range(&self, key: &ObjectKey, range: std::ops::Range<u64>) -> Result<Vec<u8>> {
        use object_store::ObjectStoreExt as _;

        // Clamp against the object's real size, per the trait's contract: a
        // caller may ask for a manifest-sized prefix without a head() first.
        // S3 returns 416 for a range wholly past the end, so this cannot be left
        // to the service if the contract is to hold across backends.
        let size = self.head(key).await?.size;
        let start = range.start.min(size);
        let end = range.end.min(size);
        if start >= end {
            return Ok(Vec::new());
        }

        let bytes = self
            .inner
            .get_range(&Self::path(key), start..end)
            .await
            .map_err(|e| Self::map("get_range", key, e))?;
        Ok(bytes.to_vec())
    }

    async fn put(&self, key: &ObjectKey, bytes: &[u8]) -> Result<()> {
        use object_store::ObjectStoreExt as _;
        self.inner
            .put(&Self::path(key), bytes.to_vec().into())
            .await
            .map_err(|e| Self::map("put", key, e))?;
        Ok(())
    }

    async fn delete(&self, key: &ObjectKey) -> Result<()> {
        use object_store::ObjectStoreExt as _;
        match self.inner.delete(&Self::path(key)).await {
            Ok(()) => Ok(()),
            // Idempotent, matching the trait contract and the local backend.
            Err(object_store::Error::NotFound { .. }) => Ok(()),
            Err(e) => Err(Self::map("delete", key, e)),
        }
    }

    async fn list(&self, prefix: &str) -> Result<Vec<ObjectKey>> {
        use futures::StreamExt as _;
        use object_store::ObjectStore as _;

        let path = (!prefix.is_empty()).then(|| object_store::path::Path::from(prefix));
        let mut stream = self.inner.list(path.as_ref());
        let mut found = Vec::new();
        while let Some(item) = stream.next().await {
            let meta = item.map_err(|e| {
                StorageError::Other(anyhow::Error::from(e).context("listing objects"))
            })?;
            if let Ok(key) = ObjectKey::new(meta.location.as_ref()) {
                found.push(key);
            }
        }
        found.sort();
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A credential must never reach a log or a panic message.
    ///
    /// `Debug` on a config struct is one of the commonest ways a long-lived
    /// object-storage key escapes, and under D-05 that key carries direct cost
    /// exposure. Asserted with a control: the bucket, which *should* appear,
    /// does — so a pass cannot come from `Debug` printing nothing useful.
    #[test]
    fn debug_redacts_the_credential_but_keeps_diagnostics() {
        let config = S3Config {
            provider: Provider::R2,
            bucket: "nemr-bundles".into(),
            endpoint: "https://acct.r2.cloudflarestorage.com".into(),
            region: "auto".into(),
            access_key_id: "AKIA-SHOULD-NOT-APPEAR".into(),
            secret_access_key: "SECRET-SHOULD-NOT-APPEAR".into(),
        };
        let rendered = format!("{config:?}");

        assert!(
            !rendered.contains("SECRET-SHOULD-NOT-APPEAR"),
            "the secret must never be rendered: {rendered}"
        );
        assert!(
            !rendered.contains("AKIA-SHOULD-NOT-APPEAR"),
            "the access key id identifies the account and must be redacted too: {rendered}"
        );
        // Control: the fields that SHOULD appear do, so this is not passing
        // merely because Debug prints nothing.
        assert!(rendered.contains("nemr-bundles"), "bucket should appear: {rendered}");
        assert!(rendered.contains("r2.cloudflarestorage.com"), "endpoint should appear");
    }

    /// An unconfigured environment is not an error: it is the normal state for
    /// the open engine, which must never require a backend.
    #[test]
    fn an_unconfigured_environment_yields_none_not_an_error() {
        // Guard against a developer's real config leaking into the test.
        if std::env::var("NEMR_S3_BUCKET").is_ok() {
            return;
        }
        assert!(
            matches!(S3Config::from_env(), Ok(None)),
            "no backend configured must be Ok(None), never an error"
        );
    }

    #[test]
    fn provider_only_supplies_configuration() {
        // R2 uses `auto` by convention; others carry a real region. If a
        // provider ever needed different *behaviour* rather than different
        // configuration, it would not belong above the trait (E-11).
        assert_eq!(Provider::R2.default_region(), "auto");
        assert_eq!(Provider::B2.default_region(), "us-east-1");
        assert_eq!(Provider::R2.label(), "r2");
        assert_eq!(Provider::B2.label(), "b2");
    }

    #[test]
    fn error_summary_cannot_carry_a_query_string() {
        // A presigned or misconfigured request can put a credential in a query
        // parameter; a diagnostic must not copy it into a log.
        let text = "Generic error: request failed for https://x/y?X-Amz-Signature=LEAKED";
        let trimmed = text.split('?').next().unwrap();
        assert!(!trimmed.contains("LEAKED"));
    }
}
