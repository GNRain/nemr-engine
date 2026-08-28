//! Per-session network namespaces (NET-02).
//!
//! # Why
//!
//! Under NET-01 every session shared rootlesskit's network namespace, so two
//! sessions could not both bind port 8000 — the second got `EADDRINUSE` from
//! its own dev server, an error that never mentioned nemr and pointed at
//! nothing. Two sessions each running a dev server on the conventional port is
//! the normal expectation; it is what Docker does.
//!
//! # How
//!
//! The OCI spec now asks runc for a network namespace, so each session gets a
//! fresh one containing only a down loopback. The engine then wires it up:
//!
//! ```text
//!   session netns            rootlesskit netns              host
//!   ceth0 10.99.N.2/24  <-->  nemrN 10.99.N.1/24  --NAT-->  tap0  -->  world
//! ```
//!
//! and a host port reaches the session in **one hop**, because rootlesskit's
//! port API takes a child IP: `127.0.0.1:8000 -> 10.99.N.2:8000`. That was the
//! spike's decisive finding — the forwarding path does not gain a second hop.
//!
//! # Where the work happens
//!
//! The daemon runs on the host, outside rootlesskit's namespaces, so every
//! command here is executed via `nsenter` into rootlesskit's user and network
//! namespaces, where we hold the capabilities to create links (`=ep`, measured).
//! The session's own namespace cannot be entered directly from the host — it
//! belongs to rootlesskit's user namespace — so it is reached by a second
//! `nsenter` from inside, addressing the container by PID.
//!
//! All of it is rootless: no `sudo`, no privileged helper, no system daemon.

use std::process::Command;

use anyhow::{bail, Context, Result};

/// The address space sessions are allocated from.
///
/// `10.99.0.0/16` gives 256 sessions a /24 each. Checked against the host's own
/// routes before use — see [`check_range_is_free`].
pub const SUBNET_PREFIX: &str = "10.99";
pub const MAX_SESSIONS: u8 = 255;

/// One session's network allocation, recorded on its container label so it
/// survives restarts and is reapplied verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Allocation {
    /// The third octet: `10.99.<index>.0/24`.
    pub index: u8,
}

impl Allocation {
    pub fn gateway(&self) -> String {
        format!("{SUBNET_PREFIX}.{}.1", self.index)
    }
    pub fn session_ip(&self) -> String {
        format!("{SUBNET_PREFIX}.{}.2", self.index)
    }
    pub fn cidr(&self) -> String {
        format!("{SUBNET_PREFIX}.{}.0/24", self.index)
    }
    /// The rootlesskit-side interface name. Kept under 15 bytes (IFNAMSIZ).
    pub fn host_link(&self) -> String {
        format!("nemr{}", self.index)
    }
}

/// Parse an allocation back from its label value.
pub fn decode_allocation(raw: &str) -> Option<Allocation> {
    raw.trim()
        .parse::<u8>()
        .ok()
        .map(|index| Allocation { index })
}

pub fn encode_allocation(a: Allocation) -> String {
    a.index.to_string()
}

/// Choose an index no other project is using.
///
/// Deterministic given the set in use, so the same inputs always give the same
/// answer and a test can pin it.
pub fn allocate_index(in_use: &[u8]) -> Result<Allocation> {
    for index in 0..=MAX_SESSIONS {
        if !in_use.contains(&index) {
            return Ok(Allocation { index });
        }
    }
    bail!(
        "no free session network remains: all {} subnets under {SUBNET_PREFIX}.0.0/16 are \
         allocated. Delete a project to free one.",
        MAX_SESSIONS as u16 + 1
    )
}

/// Refuse to allocate into a range the host already routes (Product Owner
/// ruling).
///
/// Assuming `10.99` is free is exactly the class of bug that does not error:
/// on a `10.x` corporate network or VPN the packets simply go somewhere else,
/// and nothing anywhere reports a problem. So the host's own routes are read
/// and an overlap is a refusal with the conflicting route named.
pub fn check_range_is_free() -> Result<()> {
    let out = Command::new("ip")
        .args(["-4", "route", "show"])
        .output()
        .context("reading the host's routing table to check for a subnet conflict")?;
    let table = String::from_utf8_lossy(&out.stdout);

    for line in table.lines() {
        let Some(dest) = line.split_whitespace().next() else {
            continue;
        };
        if overlaps_our_range(dest) {
            bail!(
                "this host already routes {dest}, which overlaps the {SUBNET_PREFIX}.0.0/16 \
                 range nemr allocates session networks from.\n\
                 Allocating into it would send session traffic to the wrong place \
                 SILENTLY — nothing would error, packets would simply go elsewhere.\n\
                 Refusing rather than guessing. The conflicting route is:\n    {line}"
            );
        }
    }
    Ok(())
}

/// Does a route destination overlap `10.99.0.0/16`?
///
/// Deliberately conservative: anything inside 10.99/16, and any shorter prefix
/// of 10/8 that would contain it. A false refusal is a message; a false
/// acceptance is silent misrouting.
pub fn overlaps_our_range(dest: &str) -> bool {
    let Some((addr, len)) = dest.split_once('/') else {
        // A host route with no prefix length: overlaps only if it is in 10.99.
        return dest.starts_with(&format!("{SUBNET_PREFIX}."));
    };
    let Ok(prefix) = len.parse::<u8>() else {
        return false;
    };
    let octets: Vec<&str> = addr.split('.').collect();
    if octets.len() != 4 || octets[0] != "10" {
        return false;
    }
    if prefix <= 8 {
        // 10.0.0.0/8 or shorter contains all of 10.99.
        return true;
    }
    // Between /9 and /16 the second octet decides.
    octets.get(1).and_then(|o| o.parse::<u8>().ok()) == Some(99)
}

// --- wiring, via rootlesskit's namespaces --------------------------------------

/// rootlesskit's child PID — the process whose user and network namespaces own
/// everything below.
fn rootlesskit_child_pid() -> Result<String> {
    let runtime = std::env::var("XDG_RUNTIME_DIR")
        .context("XDG_RUNTIME_DIR is not set, so rootlesskit's state cannot be located")?;
    let path = format!("{runtime}/containerd-rootless/child_pid");
    let pid = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {path}. Is rootless containerd running?"))?;
    Ok(pid.trim().to_string())
}

/// Run a command inside rootlesskit's user + network namespaces.
///
/// The host PID namespace is deliberately NOT entered: the container is
/// addressed by its host PID, which must stay resolvable.
fn in_rootlesskit(script: &str) -> Result<std::process::Output> {
    let child = rootlesskit_child_pid()?;
    Command::new("nsenter")
        .args([
            "-t",
            &child,
            "-U",
            "-n",
            "--preserve-credentials",
            "--",
            "bash",
            "-c",
            script,
        ])
        .output()
        .context("entering rootlesskit's namespaces to configure session networking")
}

fn run_checked(script: &str, what: &str) -> Result<()> {
    let out = in_rootlesskit(script)?;
    if !out.status.success() {
        bail!(
            "{what} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// Wire a freshly started session into the network.
///
/// Idempotent per session: re-running for a live session is a no-op rather than
/// an error, because `start` may be retried and the declaration is
/// authoritative.
pub fn connect_session(container_pid: u32, alloc: Allocation) -> Result<()> {
    let link = alloc.host_link();
    let gw = alloc.gateway();
    let ip = alloc.session_ip();
    let cidr = alloc.cidr();

    // One script, so a partially-applied configuration cannot be left behind by
    // a failure between steps: `set -e` stops at the first problem and the
    // caller tears the session down.
    let script = format!(
        r#"set -e
# Already wired? Then there is nothing to do.
if ip link show {link} >/dev/null 2>&1; then exit 0; fi
ip link add {link} type veth peer name ceth0
ip link set ceth0 netns {container_pid}
ip addr add {gw}/24 dev {link}
ip link set {link} up
nsenter -t {container_pid} -n -- ip addr add {ip}/24 dev ceth0
nsenter -t {container_pid} -n -- ip link set ceth0 up
nsenter -t {container_pid} -n -- ip link set lo up
nsenter -t {container_pid} -n -- ip route add default via {gw}
# Egress. Without this the session has an address and no way out — which is
# how Claude Code loses the API, so it is part of connecting, not an extra.
sysctl -w net.ipv4.ip_forward=1 >/dev/null
iptables -t nat -C POSTROUTING -s {cidr} -o tap0 -j MASQUERADE 2>/dev/null \
  || iptables -t nat -A POSTROUTING -s {cidr} -o tap0 -j MASQUERADE
"#
    );
    run_checked(&script, &format!("wiring session network {cidr}"))
}

/// Remove a session's veth and NAT rule.
///
/// Best-effort and idempotent: the veth disappears with the namespace when the
/// task dies, so the common case is that there is nothing left to remove, and
/// that must not read as a failure.
pub fn disconnect_session(alloc: Allocation) {
    let script = format!(
        "ip link del {} 2>/dev/null; iptables -t nat -D POSTROUTING -s {} -o tap0 -j MASQUERADE 2>/dev/null; true",
        alloc.host_link(),
        alloc.cidr()
    );
    let _ = in_rootlesskit(&script);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_allocation_derives_its_addresses() {
        let a = Allocation { index: 7 };
        assert_eq!(a.gateway(), "10.99.7.1");
        assert_eq!(a.session_ip(), "10.99.7.2");
        assert_eq!(a.cidr(), "10.99.7.0/24");
        assert_eq!(a.host_link(), "nemr7");
    }

    /// Interface names are capped at IFNAMSIZ-1 (15 bytes); a longer one is
    /// refused by the kernel at creation, which would fail a session start for
    /// a reason nobody would guess.
    #[test]
    fn host_link_names_fit_the_kernel_limit() {
        for index in [0u8, 9, 99, 255] {
            let name = Allocation { index }.host_link();
            assert!(name.len() <= 15, "{name} is too long for IFNAMSIZ");
        }
    }

    #[test]
    fn allocation_skips_indices_in_use() {
        assert_eq!(allocate_index(&[]).unwrap().index, 0);
        assert_eq!(allocate_index(&[0, 1, 2]).unwrap().index, 3);
        assert_eq!(allocate_index(&[1, 2]).unwrap().index, 0, "gaps are reused");
    }

    #[test]
    fn exhaustion_is_refused_with_the_reason() {
        let all: Vec<u8> = (0..=255).collect();
        let err = format!("{:#}", allocate_index(&all).unwrap_err());
        assert!(err.contains("no free session network"), "{err}");
    }

    #[test]
    fn an_allocation_round_trips_through_its_label() {
        let a = Allocation { index: 42 };
        assert_eq!(decode_allocation(&encode_allocation(a)), Some(a));
        assert_eq!(decode_allocation("not a number"), None);
    }

    /// THE ROUTE-CONFLICT GUARD. Allocating into a range the host already
    /// routes does not error — packets simply go elsewhere — so the overlap
    /// test must be conservative in the direction of refusing.
    #[test]
    fn routes_that_would_swallow_our_range_are_detected() {
        // Direct overlaps.
        assert!(overlaps_our_range("10.99.0.0/16"));
        assert!(overlaps_our_range("10.99.5.0/24"));
        // A shorter prefix of 10/8 contains all of 10.99.
        assert!(overlaps_our_range("10.0.0.0/8"));
        // A bare host address inside our range.
        assert!(overlaps_our_range("10.99.4.7"));
    }

    #[test]
    fn unrelated_routes_are_not_flagged() {
        for ok in [
            "192.168.1.0/24",
            "172.17.0.0/16",
            "10.0.2.0/24", // 10.x but not 10.99
            "10.42.0.0/16",
            "default",
            "169.254.0.0/16",
        ] {
            assert!(!overlaps_our_range(ok), "{ok} should not be flagged");
        }
    }
}
