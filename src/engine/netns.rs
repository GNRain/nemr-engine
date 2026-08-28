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
    /// The session-side end, named per session while it is still in
    /// rootlesskit's SHARED namespace. A fixed name there (it was `ceth0`)
    /// collides between sessions: one left behind by a failed wiring made every
    /// other project's `ip link add` fail with `RTNETLINK answers: File exists`,
    /// naming a project that had nothing wrong with it. Sharing the `nemr`
    /// prefix also means the teardown check sees a leak of either end.
    /// It is renamed to `ceth0` once it is inside the session.
    pub fn peer_link(&self) -> String {
        format!("nemrc{}", self.index)
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
    // `table all`, not the main table alone: a VPN or a container runtime
    // routinely installs its routes in another table, and those are the ones
    // that would swallow our range without appearing here.
    let out = Command::new("ip")
        .args(["-4", "route", "show", "table", "all"])
        .output()
        .context("reading the host's routing table to check for a subnet conflict")?;

    // A guard that cannot read its input must refuse, not pass. Ignoring the
    // exit status made an unreadable table indistinguishable from an empty one,
    // so `ip` failing (absent, denied, a kernel without the table) silently
    // turned the whole check into a no-op — the one outcome a guard must never
    // have.
    if !out.status.success() {
        bail!(
            "could not read this host's routing table ({}): {}\n\
             Refusing to allocate a session network without reading it: \"cannot tell\" \
             is not \"free\", and allocating into a conflict does not error — it \
             misroutes silently.",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let table = String::from_utf8_lossy(&out.stdout);

    if let Some(line) = conflicting_route(&table) {
        let dest = route_destination(line).unwrap_or(line);
        {
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

/// The first line of a routing table that overlaps our range, if any.
///
/// Split out from [`check_range_is_free`] so the whole-table decision is
/// testable against real `ip route show table all` output, on a host that does
/// not have the conflicting route. The guard had no test at any level, which is
/// how it carried two blind spots at once and neither showed up.
pub fn conflicting_route(table: &str) -> Option<&str> {
    table
        .lines()
        .find(|line| route_destination(line).is_some_and(overlaps_our_range))
}

/// The destination prefix from one `ip route` line.
///
/// The first token is not always the destination. Non-unicast routes are
/// printed with their type first — `unreachable 10.0.0.0/8`, `blackhole
/// 10.96.0.0/12`, `local 10.1.0.4 dev eth0` — and reading token zero classified
/// those as a destination called "unreachable", which overlaps nothing. The
/// blanket routes a VPN installs are exactly the ones that arrive in that form.
fn route_destination(line: &str) -> Option<&str> {
    const TYPES: [&str; 9] = [
        "unicast",
        "local",
        "broadcast",
        "multicast",
        "throw",
        "unreachable",
        "prohibit",
        "blackhole",
        "nat",
    ];
    let mut tokens = line.split_whitespace();
    let first = tokens.next()?;
    if TYPES.contains(&first) {
        tokens.next()
    } else {
        Some(first)
    }
}

/// Does a route destination overlap `10.99.0.0/16`?
///
/// Prefix arithmetic, not octet inspection. The octet version answered "no" for
/// every aggregate between /9 and /15 — `10.0.0.0/9`, `10.64.0.0/10` and
/// `10.98.0.0/15` all contain 10.99.0.0/16, and all read as unrelated because
/// their second octet is not 99. Those are precisely the summarised routes a
/// corporate VPN pushes, which is the case the guard was written for.
///
/// Two prefixes overlap when the shorter one contains the other's base address.
pub fn overlaps_our_range(dest: &str) -> bool {
    /// 10.99.0.0, as the kernel would hold it.
    const OURS: u32 = 0x0A_63_00_00;
    const OURS_LEN: u8 = 16;

    let (addr, len) = match dest.split_once('/') {
        Some((addr, len)) => match len.parse::<u8>() {
            Ok(len) if len <= 32 => (addr, len),
            _ => return false,
        },
        // A route with no prefix length is a single host: /32.
        None => (dest, 32u8),
    };
    // A default route contains every address and conflicts with nothing — it is
    // the fallback, not a claim on the range. Flagging it would refuse on every
    // machine that has one, which is all of them.
    if len == 0 {
        return false;
    }
    let Ok(ip) = addr.parse::<std::net::Ipv4Addr>() else {
        // "default", an interface name, a v6 literal: not a v4 prefix.
        return false;
    };

    let shorter = len.min(OURS_LEN);
    let mask = u32::MAX << (32 - shorter);
    (u32::from(ip) & mask) == (OURS & mask)
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
        // Both streams and the status: `set -e` reports the failing command on
        // stderr, but `ip` and `iptables` put some of their diagnosis on stdout,
        // and a bare "failed: " with an empty stderr says nothing at all.
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        bail!(
            "{what} failed ({}).\n  stderr: {}\n  stdout: {}",
            out.status,
            if stderr.trim().is_empty() {
                "(empty)"
            } else {
                stderr.trim()
            },
            if stdout.trim().is_empty() {
                "(empty)"
            } else {
                stdout.trim()
            },
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
    let peer = alloc.peer_link();
    let gw = alloc.gateway();
    let ip = alloc.session_ip();
    let cidr = alloc.cidr();

    // One script, `set -e`, and a cleanup trap: a failure between steps removes
    // the pair rather than leaving it for the next start to misread.
    let script = format!(
        r#"set -e
# Start from a clean slate rather than trusting a NAME.
#
# This used to be `if ip link show {link}; then exit 0; fi`. A link with that
# name is not evidence that a session is wired — a wiring that failed halfway
# leaves exactly that, and the session it belonged to is gone. So the guard
# turned every leaked link into a session that starts, reports success and has
# no network at all. Deleting the pair removes both ends wherever they are, and
# a freshly started task's namespace is empty by construction, so rebuilding
# from scratch is what idempotence actually means here.
ip link del {link} >/dev/null 2>&1 || true

# Anything below that fails takes the pair with it. Without this a half-applied
# wiring left `{peer}` in the SHARED namespace, and every other project's start
# then failed with "RTNETLINK answers: File exists", naming a project that had
# nothing wrong with it.
trap 'ip link del {link} >/dev/null 2>&1 || true' EXIT

ip link add {link} type veth peer name {peer}
ip link set {peer} netns {container_pid}
ip addr add {gw}/24 dev {link}
ip link set {link} up
# Inside the session it is `ceth0`: conventional, and the per-session name only
# needs to be unique while the end is still in the shared namespace.
nsenter -t {container_pid} -n -- ip link set {peer} name ceth0
nsenter -t {container_pid} -n -- ip addr add {ip}/24 dev ceth0
nsenter -t {container_pid} -n -- ip link set ceth0 up
nsenter -t {container_pid} -n -- ip link set lo up
nsenter -t {container_pid} -n -- ip route add default via {gw}
# Egress. Without this the session has an address and no way out — which is
# how Claude Code loses the API, so it is part of connecting, not an extra.
sysctl -w net.ipv4.ip_forward=1 >/dev/null
# -w: wait for the xtables lock rather than failing on it. Inert on the hosts
# measured here — Ubuntu 22.04/24.04 use the nft backend, which strace shows
# never opens /run/xtables.lock — so this is insurance for a legacy-backend
# host, not a fix for anything observed. Said plainly rather than claimed as a
# cure, because a comment that overstates what a flag does is how the next
# person stops trusting the comments.
iptables -w 5 -t nat -C POSTROUTING -s {cidr} -o tap0 -j MASQUERADE 2>/dev/null \
  || iptables -w 5 -t nat -A POSTROUTING -s {cidr} -o tap0 -j MASQUERADE

# Read the result back from inside the session before calling it wired. Every
# command above can succeed and still leave a namespace that reaches nothing;
# reporting success on the strength of the commands rather than the outcome is
# how a session ends up looking started and being unreachable.
nsenter -t {container_pid} -n -- ip -4 addr show ceth0 | grep -q "inet {ip}/24"
nsenter -t {container_pid} -n -- ip -4 route show | grep -q "^default via {gw}"

trap - EXIT
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
    // Each removal tolerates absence, but the SCRIPT reports whether it ran at
    // all. Discarding the outcome entirely meant a teardown that could not enter
    // rootlesskit's namespace — the one failure that leaves every rule behind —
    // was indistinguishable from a clean removal with nothing to do.
    let script = format!(
        "ip link del {} >/dev/null 2>&1 || true\n\
         iptables -w 5 -t nat -D POSTROUTING -s {} -o tap0 -j MASQUERADE >/dev/null 2>&1 || true\n\
         # Anything of ours still here is a leak, and saying so is the point.\n\
         ip -brief link show | grep -E \"^nemrc?{}[@ ]\" || true\n\
         iptables -w 5 -t nat -S POSTROUTING | grep -- \"-s {} \" || true\n",
        alloc.host_link(),
        alloc.cidr(),
        alloc.index,
        alloc.cidr(),
    );
    match in_rootlesskit(&script) {
        Ok(out) if out.status.success() => {
            let left = String::from_utf8_lossy(&out.stdout);
            if !left.trim().is_empty() {
                eprintln!(
                    "[nemr:netns] session network {} did not fully release; still present:\n{}",
                    alloc.cidr(),
                    left.trim()
                );
            }
        }
        Ok(out) => eprintln!(
            "[nemr:netns] could not release session network {} ({}): {}",
            alloc.cidr(),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ),
        Err(e) => eprintln!(
            "[nemr:netns] could not enter rootlesskit's namespace to release {}: {e:#}",
            alloc.cidr()
        ),
    }
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
    ///
    /// The length assertion alone cannot fail: the index is a `u8`, so both
    /// names are at most 7 bytes for every input the type permits — it was a
    /// tautology wearing a guard's name. What can change is the PREFIX, so the
    /// budget is asserted against the prefix that is actually in use, and the
    /// two names are pinned to be distinct (they share one namespace).
    #[test]
    fn host_link_names_fit_the_kernel_limit() {
        const IFNAMSIZ_MAX: usize = 15;
        for index in [0u8, 9, 99, 255] {
            let a = Allocation { index };
            assert!(a.host_link().len() <= IFNAMSIZ_MAX, "{}", a.host_link());
            assert!(a.peer_link().len() <= IFNAMSIZ_MAX, "{}", a.peer_link());
            assert_ne!(
                a.host_link(),
                a.peer_link(),
                "both ends of the veth live in rootlesskit's namespace until the \
                 peer is moved, so they cannot share a name"
            );
        }
        // The real budget: whatever prefix the names are built from must leave
        // room for the widest index. This is what a rename would break.
        let widest = Allocation { index: u8::MAX };
        let prefix_budget = IFNAMSIZ_MAX - u8::MAX.to_string().len();
        assert!(
            widest.host_link().len() - u8::MAX.to_string().len() <= prefix_budget,
            "the link-name prefix leaves no room for index 255"
        );
        assert!(
            widest.peer_link().len() - u8::MAX.to_string().len() <= prefix_budget,
            "the peer-name prefix leaves no room for index 255"
        );
    }

    /// Two sessions must never be handed the same name in the shared namespace.
    #[test]
    fn every_index_gets_its_own_pair_of_names() {
        let mut seen = std::collections::HashSet::new();
        for index in 0..=u8::MAX {
            let a = Allocation { index };
            assert!(seen.insert(a.host_link()), "duplicate host link at {index}");
            assert!(seen.insert(a.peer_link()), "duplicate peer link at {index}");
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

    /// THE AGGREGATES BETWEEN /9 AND /15. Every one of these contains
    /// 10.99.0.0/16 and none has 99 in its second octet, so the octet-comparing
    /// version answered "no" to all of them — while a summarised VPN route is
    /// exactly this shape. Each case is checked against the arithmetic in the
    /// comment so the expectation is not simply what the code happens to say.
    #[test]
    fn summarised_ten_routes_that_contain_our_range_are_detected() {
        for (route, covers) in [
            ("10.0.0.0/9", "10.0.0.0 - 10.127.255.255"),
            ("10.64.0.0/10", "10.64.0.0 - 10.127.255.255"),
            ("10.96.0.0/11", "10.96.0.0 - 10.127.255.255"),
            ("10.96.0.0/12", "10.96.0.0 - 10.111.255.255"),
            ("10.96.0.0/13", "10.96.0.0 - 10.103.255.255"),
            ("10.96.0.0/14", "10.96.0.0 - 10.99.255.255"),
            ("10.98.0.0/15", "10.98.0.0 - 10.99.255.255"),
        ] {
            assert!(
                overlaps_our_range(route),
                "{route} covers {covers}, which contains 10.99.0.0/16"
            );
        }
    }

    /// The neighbours of each of those, one bit away and genuinely disjoint.
    /// Without these the guard could pass by refusing everything.
    #[test]
    fn ten_routes_that_do_not_contain_our_range_are_not_flagged() {
        for (route, covers) in [
            ("10.128.0.0/9", "10.128.0.0 - 10.255.255.255"),
            ("10.0.0.0/10", "10.0.0.0 - 10.63.255.255"),
            ("10.64.0.0/12", "10.64.0.0 - 10.79.255.255"),
            ("10.100.0.0/14", "10.100.0.0 - 10.103.255.255"),
            ("10.96.0.0/15", "10.96.0.0 - 10.97.255.255"),
            ("10.98.0.0/16", "10.98.0.0 - 10.98.255.255"),
            ("10.100.0.0/16", "10.100.0.0 - 10.100.255.255"),
        ] {
            assert!(
                !overlaps_our_range(route),
                "{route} covers {covers}, which does NOT contain 10.99.0.0/16"
            );
        }
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
            // A default route contains everything and conflicts with nothing.
            "0.0.0.0/0",
            // Not a v4 prefix at all: an interface name, a nonsense length.
            "eth0",
            "10.99.0.0/99",
        ] {
            assert!(!overlaps_our_range(ok), "{ok} should not be flagged");
        }
    }

    /// `ip route` prints non-unicast routes with the TYPE first, and the
    /// blanket routes a VPN installs arrive in exactly that form. Reading token
    /// zero classified them as a destination called "unreachable".
    #[test]
    fn route_lines_are_read_past_their_type_keyword() {
        assert_eq!(
            route_destination("unreachable 10.0.0.0/8 dev lo metric 1024"),
            Some("10.0.0.0/8")
        );
        assert_eq!(
            route_destination("blackhole 10.96.0.0/12"),
            Some("10.96.0.0/12")
        );
        assert_eq!(
            route_destination("local 10.99.0.1 dev nemr0 table local"),
            Some("10.99.0.1")
        );
        // A plain unicast line still reads as itself.
        assert_eq!(
            route_destination("10.99.0.0/24 dev nemr0 proto kernel scope link"),
            Some("10.99.0.0/24")
        );
        assert_eq!(
            route_destination("default via 10.1.0.1 dev eth0"),
            Some("default")
        );
        assert_eq!(route_destination(""), None);
    }

    /// The guard, against a routing table shaped like a real one — a GitHub
    /// runner's, which is where this code runs in CI. Both blind spots are
    /// represented: an aggregate rather than an exact match, and a route
    /// carrying a type keyword.
    #[test]
    fn the_guard_reads_a_whole_routing_table() {
        let clean = "\
default via 10.1.0.1 dev eth0 proto dhcp src 10.1.0.4 metric 100
10.1.0.0/16 dev eth0 proto kernel scope link src 10.1.0.4
168.63.129.16 via 10.1.0.1 dev eth0 proto dhcp metric 100
local 10.1.0.4 dev eth0 table local proto kernel scope host src 10.1.0.4
broadcast 127.255.255.255 dev lo table local proto kernel scope link src 127.0.0.1
";
        assert_eq!(
            conflicting_route(clean),
            None,
            "a runner's own table must not be refused, or nothing could ever be created"
        );

        let vpn = format!("{clean}10.64.0.0/10 dev tun0 scope link\n");
        assert_eq!(
            conflicting_route(&vpn),
            Some("10.64.0.0/10 dev tun0 scope link")
        );

        let typed = format!("{clean}unreachable 10.0.0.0/8 dev lo metric 1024\n");
        assert_eq!(
            conflicting_route(&typed),
            Some("unreachable 10.0.0.0/8 dev lo metric 1024")
        );

        // Our OWN session routes must not read as a conflict once they exist —
        // they live in rootlesskit's namespace, not the host's, but a future
        // change that put them on the host would make every create refuse.
        let ours = format!("{clean}10.99.3.0/24 dev nemr3 proto kernel scope link\n");
        assert!(conflicting_route(&ours).is_some());
    }

    /// The two halves together: a type-prefixed summarised route is the case
    /// that slipped through BOTH defects at once, and it is the realistic one.
    #[test]
    fn a_type_prefixed_summarised_route_is_caught() {
        let line = "unreachable 10.0.0.0/9 dev lo proto static metric 1024";
        let dest = route_destination(line).expect("a destination");
        assert!(
            overlaps_our_range(dest),
            "{line} would have been read as an unrelated route by both halves"
        );
    }
}
