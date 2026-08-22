//! containerd leases — holding a resource alive while it is being built.
//!
//! # Why this module exists (F-63)
//!
//! containerd's garbage collector deletes any resource nothing refers to. A
//! snapshot becomes referenced when a container record naming it is written, so
//! between `PrepareSnapshot` and `CreateContainer` the snapshot is, by
//! containerd's definition, garbage. If the collector runs in that window the
//! snapshot is deleted — and the record write still succeeds, because
//! containerd does not validate that `snapshot_key` resolves. The result is a
//! container that reports created and can never start.
//!
//! A lease is containerd's answer to exactly this. It is a named holder that
//! counts as a reference, so anything created while the lease is in scope
//! survives until the lease is released. This is the mechanism containerd's own
//! client uses for the same sequence — adopting it, not working around a quirk.
//!
//! # The expiry label is not optional
//!
//! A lease that is never released pins its resources forever, so a process that
//! dies mid-create would leak a snapshot the collector can no longer touch —
//! the opposite failure, and a worse one, because nothing reports it. Every
//! lease here carries `containerd.io/gc.expire`, which containerd honours by
//! dropping the lease once the timestamp passes. The window is generous enough
//! that a slow create never loses its lease, and short enough that a leak
//! self-heals without operator involvement.

use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use containerd_client::services::v1::{CreateRequest, DeleteRequest};
use containerd_client::tonic::Request;
use containerd_client::with_namespace;

use super::client::ContainerdClient;

/// How long a lease survives without being released.
///
/// Only a backstop for a crashed process: the normal path releases explicitly
/// within a second or two. One hour is far beyond any legitimate create and far
/// below "forever", which is what no label at all would mean.
const LEASE_TTL: Duration = Duration::from_secs(3600);

/// The gRPC metadata key containerd reads a lease id from.
///
/// Same header its Go client sets via `leases.WithLease`. Requests carrying it
/// attribute anything they create to that lease.
pub const LEASE_HEADER: &str = "containerd-lease";

/// Build a request carrying both the namespace and a lease.
///
/// `with_namespace!` alone is not enough: without the lease header containerd
/// attributes the created resource to nothing, which is the F-63 window.
#[macro_export]
macro_rules! with_lease {
    ($req:expr, $ns:expr, $lease:expr) => {{
        let mut req = $crate::__reexport::Request::new($req);
        let md = req.metadata_mut();
        md.insert("containerd-namespace", $ns.parse().unwrap());
        md.insert($crate::leases::LEASE_HEADER, $lease.parse().unwrap());
        req
    }};
}

/// A lease that must be released by its caller.
///
/// Deliberately **not** `Drop`-based: releasing is an async gRPC call and Rust
/// has no async drop, so a `Drop` impl could only fire-and-forget or block. The
/// caller releases explicitly on both the success and the failure path, which
/// is visible in the code rather than implied by a destructor that cannot
/// report errors.
#[derive(Debug, Clone)]
pub struct Lease {
    id: String,
}

impl Lease {
    pub fn id(&self) -> &str {
        &self.id
    }
}

impl ContainerdClient {
    /// Take a lease that protects everything created under it.
    pub async fn create_lease(&self, id: &str) -> Result<Lease> {
        let expires_at = SystemTime::now() + LEASE_TTL;
        let expires_at = humantime::format_rfc3339_seconds(expires_at).to_string();

        let request = CreateRequest {
            id: id.to_string(),
            labels: [("containerd.io/gc.expire".to_string(), expires_at)]
                .into_iter()
                .collect(),
        };

        self.raw()
            .leases()
            .create(with_namespace!(request, self.namespace()))
            .await
            .with_context(|| format!("failed to create containerd lease {id:?}"))?;

        Ok(Lease { id: id.to_string() })
    }

    /// Release a lease. Whatever it was protecting becomes ordinary garbage,
    /// which is correct once a real reference exists.
    ///
    /// Absent is success: a lease already expired or already deleted needs no
    /// action, and failing here would turn cleanup into an error on a path that
    /// has otherwise succeeded.
    pub async fn delete_lease(&self, lease: &Lease) -> Result<()> {
        let request = DeleteRequest {
            id: lease.id.clone(),
            sync: false,
        };
        match self
            .raw()
            .leases()
            .delete(with_namespace!(request, self.namespace()))
            .await
        {
            Ok(_) => Ok(()),
            Err(status) if status.code() == containerd_client::tonic::Code::NotFound => Ok(()),
            Err(status) => {
                Err(status).with_context(|| format!("failed to delete lease {:?}", lease.id))
            }
        }
    }

    /// Run a garbage collection and wait for it to finish.
    ///
    /// containerd exposes no "collect now" call, but a lease deleted with
    /// `sync = true` is answered only after a collection has completed — so
    /// creating a lease and sync-deleting it is a collection on demand. Used by
    /// the F-63 guard to make a race deterministic; best-effort, since a probe
    /// that cannot run should not fail the caller.
    #[doc(hidden)]
    pub async fn collect_garbage_now(&self) {
        let id = format!("nemr-gc-trigger-{}", std::process::id());
        let Ok(lease) = self.create_lease(&id).await else {
            return;
        };
        let request = DeleteRequest {
            id: lease.id.clone(),
            sync: true,
        };
        let _ = self
            .raw()
            .leases()
            .delete(with_namespace!(request, self.namespace()))
            .await;
    }

    /// Lease ids currently held, for diagnostics and leak checks.
    pub async fn list_lease_ids(&self) -> Result<Vec<String>> {
        use containerd_client::services::v1::ListRequest;
        let response = self
            .raw()
            .leases()
            .list(with_namespace!(
                ListRequest { filters: vec![] },
                self.namespace()
            ))
            .await
            .context("containerd ListLeases request failed")?;
        Ok(response
            .into_inner()
            .leases
            .into_iter()
            .map(|lease| lease.id)
            .collect())
    }
}
