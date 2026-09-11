//! The nemrd control-plane API (E-09), as a crate of its own.
//!
//! Why a crate: the daemon is the single writer to containerd, and everything
//! else — the open CLI, the commercial process that serves the UI — is a gRPC
//! client of it. Before this crate the generated client lived inside the
//! engine, so a client had to link the whole engine (the containerd wrapper,
//! the volume layer, the bundle code) to talk to the daemon. Now a client
//! links this: the proto, the generated stubs, the socket path, and the
//! connect-and-handshake path with its audit stream.
//!
//! This is the seam the E-11 ruling of 2026-09-06 named: open, like the engine
//! and the wrapper (`scripts/check_seam.sh` scans it), and the commercial
//! half depends on it in the allowed direction.

/// The nemrd control-plane gRPC service (E-09), generated from
/// `proto/nemr.proto` at build time.
pub mod proto {
    tonic::include_proto!("nemr.v1");

    /// The wire protocol version. Bump on ANY incompatible change to the
    /// service. The daemon refuses a client whose version differs (the
    /// hash-gate lesson applied to the protocol).
    // v2: adds Provision (F-118). Bumped so a new CLI against an old daemon —
    // or the reverse — refuses with the reinstall advice instead of failing
    // with an unimplemented-RPC error that names nothing.
    // v3: adds Adopt (E-23).
    // v4: adds SizeLimits, and `CreateRequest.size` widens from one of three
    // preset words to any size in range. The FIELD is unchanged — still a
    // string, still "2GB" for the same volume — so the message is wire-
    // compatible both ways; what is not compatible is the new RPC, which an
    // older daemon answers with UNIMPLEMENTED. The handshake turns that into a
    // refusal that names the fix, which is the whole reason it exists.
    pub const PROTOCOL_VERSION: u32 = 4;
}

pub mod client;
pub mod socket;
pub mod userns;
