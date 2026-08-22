//! Connection management and the shared client handle.
//!
//! This is the only place a containerd connection is opened.
//! Everything above it (`src/engine/`) receives a [`ContainerdClient`] and
//! never constructs a `containerd_client::Client` of its own — see Section 3.2.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use containerd_client::Client;

/// containerd namespace this client operates in.
///
/// containerd partitions all resources by namespace; `ctr` defaults to
/// `default`, so using the same name keeps engine state inspectable with the
/// stock CLI, which several acceptance criteria rely on.
pub const DEFAULT_NAMESPACE: &str = "default";

/// Environment variable honoured by `ctr` and by this engine to override the
/// socket path.
pub const ADDRESS_ENV: &str = "CONTAINERD_ADDRESS";

/// A connected containerd client, plus the context needed to interpret it.
///
/// The socket path is retained rather than discarded after connecting: which
/// containerd instance answered is load-bearing information under PRIV-01
/// (Section 3.7), because a root-owned system daemon may also exist on the
/// host. Callers surface it so it is auditable.
pub struct ContainerdClient {
    inner: Client,
    socket_path: PathBuf,
    namespace: String,
    /// Test-only seam: awaited between `PrepareSnapshot` and the container
    /// record write inside [`ContainerdClient::create_container`].
    ///
    /// # Why a hook instead of a probabilistic test (F-63)
    ///
    /// The window this guards is two consecutive gRPC calls wide. Measured
    /// against the unfixed code under heavy concurrent churn, a create loses its
    /// snapshot about **1.25% of the time** (79 of 80 survived), so a test that
    /// races the collector would miss the defect far more often than it caught
    /// it — a guard that passes when the property is violated, which this
    /// project treats as worse than no guard.
    ///
    /// With the hook, a test triggers a synchronous collection *inside* the
    /// window and the outcome is deterministic: leased survives, unleased does
    /// not, every run.
    ///
    /// It is `None` on every constructor. Nothing reads an environment variable
    /// to enable it and there is no production path that sets it, so it cannot
    /// fire outside a test that deliberately installs it.
    #[doc(hidden)]
    pub(crate) inside_create_window: Option<CreateWindowHook>,
}

/// See [`ContainerdClient::inside_create_window`].
#[doc(hidden)]
pub type CreateWindowHook = std::sync::Arc<
    dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync,
>;

impl ContainerdClient {
    /// Install the test seam described on [`Self::inside_create_window`].
    ///
    /// Hidden from the docs and used only by the F-63 regression test. Kept on
    /// the client rather than behind `cfg(test)` because the test lives in a
    /// different crate, and behind a constructor rather than an environment
    /// variable so nothing outside this call can turn it on.
    #[doc(hidden)]
    pub fn with_create_window_hook(mut self, hook: CreateWindowHook) -> Self {
        self.inside_create_window = Some(hook);
        self
    }
}

impl ContainerdClient {
    /// Resolve the rootless containerd socket path.
    ///
    /// Order of precedence:
    /// 1. `$CONTAINERD_ADDRESS`, matching `ctr`'s own override.
    /// 2. `$XDG_RUNTIME_DIR/containerd/containerd.sock` — the rootless socket
    ///    (Section 3.7, PRIV-01).
    ///
    /// Deliberately does **not** fall back to `/run/containerd/containerd.sock`.
    /// That is the root-owned system daemon; silently connecting to it would
    /// violate PRIV-01 and would invalidate any acceptance evidence gathered
    /// against it. A missing `XDG_RUNTIME_DIR` is an error, not a cue to guess.
    pub fn default_socket_path() -> Result<PathBuf> {
        if let Some(addr) = std::env::var_os(ADDRESS_ENV) {
            return Ok(PathBuf::from(addr));
        }

        let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR").context(
            "XDG_RUNTIME_DIR is not set, so the rootless containerd socket cannot be located. \
             Set CONTAINERD_ADDRESS to the rootless socket explicitly, or run from a normal \
             login session. See PREREQUISITES.md.",
        )?;

        Ok(PathBuf::from(runtime_dir)
            .join("containerd")
            .join("containerd.sock"))
    }

    /// Connect to the rootless containerd instance in the default namespace.
    pub async fn connect() -> Result<Self> {
        let path = Self::default_socket_path()?;
        Self::connect_with(path, DEFAULT_NAMESPACE).await
    }

    /// Connect to a specific socket and namespace.
    pub async fn connect_with(
        socket_path: impl AsRef<Path>,
        namespace: impl Into<String>,
    ) -> Result<Self> {
        let socket_path = socket_path.as_ref().to_path_buf();

        // Check the socket exists first. Without this, a missing socket surfaces
        // as an opaque tonic transport error; PRIV-01 makes "wrong or absent
        // socket" a likely enough mistake to be worth naming precisely.
        if !socket_path.exists() {
            anyhow::bail!(
                "containerd socket not found at {}. Is the rootless service running? \
                 Check `systemctl --user is-active containerd-rootless.service` \
                 (see PREREQUISITES.md).",
                socket_path.display()
            );
        }

        let inner = Client::from_path(&socket_path).await.with_context(|| {
            format!(
                "failed to connect to containerd at {}",
                socket_path.display()
            )
        })?;

        Ok(Self {
            inner,
            socket_path,
            namespace: namespace.into(),
            inside_create_window: None,
        })
    }

    /// The socket this client is connected to.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// The containerd namespace this client operates in.
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// Snapshotter used for container rootfs.
    ///
    /// Confirmed `ok` rootless on the reference host at Milestone 1. A host
    /// where overlayfs is unavailable in a user namespace would need `native`,
    /// which is why this is reachable rather than inlined at call sites.
    pub fn snapshotter(&self) -> &str {
        crate::config::SNAPSHOTTER
    }

    /// Access to the underlying low-level client.
    ///
    /// Crate-visible on purpose: this is the seam Section 3.2 draws. Sibling
    /// modules in `src/containerd/` build higher-level operations on it;
    /// `src/engine/` cannot reach it.
    pub(crate) fn raw(&self) -> &Client {
        &self.inner
    }
}
