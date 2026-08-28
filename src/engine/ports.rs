//! Port forwarding: reaching a server inside a session from the host.
//!
//! # Why this exists
//!
//! A session's ports are trapped in rootlesskit's network namespace. Someone
//! runs `npm run dev`, opens a browser, and gets connection-refused — the
//! day-one shape of "this tool doesn't work". Docker's `-p` exists for exactly
//! this expectation.
//!
//! # How it works, and why it is one hop
//!
//! Project containers **share** rootlesskit's network namespace (NET-01 —
//! measured, not assumed: the `net:` inode of a container's PID 1 equals that of
//! rootlesskit's child). So a port listening inside a session is already
//! listening in rootlesskit's namespace, and one forward from the host into that
//! namespace reaches it. There is no second hop.
//!
//! rootlesskit exposes a REST API on a unix socket for exactly this, and
//! `rootlessctl` is its client. Forwards can be added and removed while the
//! session runs, which is what a dev workflow actually looks like.
//!
//! `--disable-host-loopback` (set on our unit) does **not** block this: it stops
//! the *child* reaching the host's loopback, and is not in the inbound path.
//! Verified end to end with that flag active.
//!
//! # Forwards are derived state
//!
//! The project's label is authoritative; the live forward set is derived. They
//! can disagree — rootlesskit restarts (a reboot, a crash) drop every forward,
//! and a stray `rootlessctl remove-ports` drops one. When they disagree the
//! label wins and is re-applied, the same precedence the volume layer already
//! has. Concretely: forwards are torn down on `stop` and re-created on `start`,
//! so a stopped project never holds a host port against another project while
//! the declaration still belongs to the project across runs.

use std::process::Command;

use anyhow::{bail, Context, Result};

/// Where a forward binds on the host.
///
/// Loopback by default: a dev server should not become reachable by everything
/// on the network because someone forwarded a port. `--expose` opts out, loudly.
pub const LOOPBACK: &str = "127.0.0.1";
pub const ALL_INTERFACES: &str = "0.0.0.0";

/// One forward: a host port into the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortForward {
    /// Host bind address — `127.0.0.1` unless exposed.
    pub host_ip: String,
    pub host_port: u16,
    pub container_port: u16,
}

impl PortForward {
    /// The rootlesskit spec form: `127.0.0.1:8000:8000/tcp`.
    pub fn spec(&self) -> String {
        format!(
            "{}:{}:{}/tcp",
            self.host_ip, self.host_port, self.container_port
        )
    }

    /// What a user should open. The whole point of the feature is answering
    /// "what's my URL", so the type knows how to say it.
    pub fn url(&self) -> String {
        // 0.0.0.0 is a bind address, not somewhere to point a browser.
        let host = if self.host_ip == ALL_INTERFACES {
            LOOPBACK
        } else {
            &self.host_ip
        };
        format!("http://{host}:{}", self.host_port)
    }

    /// Is this forward reachable from anywhere but this machine?
    ///
    /// Defined as **not loopback**, not as "equals 0.0.0.0" (F-98). The old
    /// string test meant an explicit `10.0.2.15:9000:8000` — a real NIC, and so
    /// LAN-reachable — reported itself as unexposed, and every surface told the
    /// user it was private. An address that will not parse is treated as
    /// exposed: the safe direction for a warning.
    pub fn exposed(&self) -> bool {
        !self
            .host_ip
            .parse::<std::net::IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false)
    }
}

/// Parse a user-supplied port argument.
///
/// Accepted, deliberately in this order of increasing explicitness:
///   `8000`                  → 127.0.0.1:8000 → 8000
///   `9000:8000`             → 127.0.0.1:9000 → 8000
///   `0.0.0.0:9000:8000`     → explicit bind address
///
/// `expose` upgrades the default bind to all interfaces; an explicit address in
/// the spec always wins over it, so the written form is never overridden by a
/// flag the reader cannot see.
pub fn parse_port_arg(arg: &str, expose: bool) -> Result<PortForward> {
    let default_ip = if expose { ALL_INTERFACES } else { LOOPBACK };
    let arg = arg.trim().trim_end_matches("/tcp");
    if arg.is_empty() {
        bail!("a port is required, e.g. 8000 or 9000:8000");
    }

    let parts: Vec<&str> = arg.split(':').collect();
    let (ip, host_s, container_s) = match parts.as_slice() {
        [p] => (default_ip.to_string(), *p, *p),
        [h, c] => (default_ip.to_string(), *h, *c),
        [ip, h, c] => ((*ip).to_string(), *h, *c),
        _ => bail!(
            "cannot read {arg:?} as a port. Use 8000, or 9000:8000 (host:container), \
             or 0.0.0.0:9000:8000"
        ),
    };

    // The bind address decides who can reach the session, so it is validated
    // rather than passed through (F-98). Unvalidated, an empty slot bound every
    // interface, "localhost" and "0" were stored and then refused by rootlesskit
    // forever, and a NIC address exposed the port to the LAN — none of them
    // warned, because exposure was a string comparison against "0.0.0.0".
    //
    // A malformed address also shifts rootlessctl's output columns, which made
    // the live forward unparseable and therefore untearable-down.
    let parsed: std::net::IpAddr = ip.parse().map_err(|_| {
        anyhow::anyhow!(
            "{ip:?} is not an IP address to bind. Use {LOOPBACK} (the default), \
             {ALL_INTERFACES} for every interface, or a specific address of this \
             host. Names like \"localhost\" are not accepted — the address is \
             passed to the network layer verbatim."
        )
    })?;

    let host_port = parse_one(host_s, "host")?;
    let container_port = parse_one(container_s, "container")?;
    Ok(PortForward {
        // Normalised, so the label and rootlesskit see one canonical spelling.
        host_ip: parsed.to_string(),
        host_port,
        container_port,
    })
}

fn parse_one(value: &str, which: &str) -> Result<u16> {
    let n: u32 = value
        .parse()
        .with_context(|| format!("{which} port {value:?} is not a number"))?;
    if n == 0 {
        bail!("{which} port must be between 1 and 65535, not 0");
    }
    if n > 65535 {
        bail!("{which} port {n} is above the maximum of 65535");
    }
    // Below 1024 the host bind needs privilege we deliberately do not have
    // (PRIV-01). Saying so beats an opaque permission error from rootlesskit.
    Ok(n as u16)
}

/// A neighbouring host port to suggest when one is taken.
///
/// Saturating, and never 0: `host_port + 1` panicked in debug on 65535 and
/// wrapped to an unusable 0 in release, so the remedy the error printed was
/// itself invalid at the top of the range.
pub fn suggest_alternative(host_port: u16) -> u16 {
    match host_port.checked_add(1) {
        Some(p) => p,
        None => host_port - 1,
    }
}

/// Serialise a project's forwards for the container label.
pub fn encode_label(ports: &[PortForward]) -> String {
    ports.iter().map(|p| p.spec()).collect::<Vec<_>>().join(",")
}

/// Read a project's forwards back from the container label.
///
/// Unparseable entries are skipped rather than failing the whole read: a label
/// written by a newer version must not make `list` or `stop` unusable on an
/// older one.
pub fn decode_label(raw: &str) -> Vec<PortForward> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|s| parse_port_arg(s, false).ok())
        .collect()
}

// --- talking to rootlesskit -------------------------------------------------

/// Where rootlesskit listens for port-management requests.
///
/// `$XDG_RUNTIME_DIR/containerd-rootless/api.sock`, matching the `--state-dir`
/// our systemd unit passes. Overridable so a test can point at a different
/// instance rather than mutating the developer's live one.
pub fn api_socket() -> Result<String> {
    if let Ok(explicit) = std::env::var("NEMR_ROOTLESSKIT_SOCKET") {
        if !explicit.is_empty() {
            return Ok(explicit);
        }
    }
    let runtime = std::env::var("XDG_RUNTIME_DIR")
        .context("XDG_RUNTIME_DIR is not set, so rootlesskit's API socket cannot be located")?;
    Ok(format!("{runtime}/containerd-rootless/api.sock"))
}

/// A forward as rootlesskit currently holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveForward {
    pub id: u32,
    pub host_ip: String,
    pub host_port: u16,
    pub container_port: u16,
}

/// Why an `add` failed, distinguished because the remedies differ.
#[derive(Debug)]
pub enum AddFailure {
    /// Another forward of ours already binds this host port. We can name the
    /// project, because we own the mapping from forward to project.
    HeldByAForward { detail: String },
    /// Something else on this host holds it — another program entirely, which
    /// we can neither name nor move.
    HeldByTheHost { detail: String },
    /// Anything else (rootlesskit unreachable, malformed spec).
    Other { detail: String },
}

impl std::fmt::Display for AddFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AddFailure::HeldByAForward { detail }
            | AddFailure::HeldByTheHost { detail }
            | AddFailure::Other { detail } => write!(f, "{detail}"),
        }
    }
}

/// Classify rootlesskit's refusal.
///
/// It reports the two cases differently and we keep them apart all the way to
/// the user, because "another of your projects has it" and "something else on
/// this machine has it" need different things done about them.
pub fn classify_add_error(stderr: &str) -> AddFailure {
    let detail = stderr.trim().to_string();
    if detail.contains("conflict with ID") {
        AddFailure::HeldByAForward { detail }
    } else if detail.contains("address already in use") {
        AddFailure::HeldByTheHost { detail }
    } else {
        AddFailure::Other { detail }
    }
}

/// Is this host address free to bind right now?
///
/// Declaring a port on a *stopped* project does not bind anything, so without
/// this the conflict would be discovered only at `start` — long after the user
/// could act on it, and reported as a warning they may not read. A test bind is
/// the honest way to ask, and it disturbs nothing: the socket is closed
/// immediately.
///
/// Inherently a point-in-time answer — the port can be taken between this check
/// and the real bind — so `start` still reports a failure if one occurs. This
/// makes the common case immediate, not the race impossible.
pub fn host_port_is_free(host_ip: &str, host_port: u16) -> std::result::Result<(), String> {
    use std::net::TcpListener;
    // Binding 0.0.0.0 also conflicts with a loopback-only holder, which is the
    // behaviour we want: --expose must not appear to succeed where a plain
    // forward would be refused.
    match TcpListener::bind((host_ip, host_port)) {
        Ok(listener) => {
            drop(listener);
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            // Marked so the caller can say "by something else on this machine"
            // ONLY for a genuine conflict, and not append it to a permission
            // failure, which is a different problem with a different remedy.
            Err(format!("IN_USE:{host_ip}:{host_port} is already in use"))
        }
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Err(format!(
            "binding {host_ip}:{host_port} was refused: {e}. Ports below 1024 \
             need privilege this engine deliberately does not have (PRIV-01)"
        )),
        Err(e) => Err(format!("cannot bind {host_ip}:{host_port}: {e}")),
    }
}

fn rootlessctl(args: &[String]) -> Result<std::process::Output> {
    let socket = api_socket()?;
    Command::new("rootlessctl")
        .arg(format!("--socket={socket}"))
        .args(args)
        .output()
        .with_context(|| {
            format!(
                "running rootlessctl against {socket}. It ships with rootlesskit; \
                 see PREREQUISITES.md if it is missing."
            )
        })
}

/// Every forward rootlesskit currently holds — across all projects, since the
/// namespace is shared. Used to detect collisions and to reconcile.
pub fn list_live() -> Result<Vec<LiveForward>> {
    let out = rootlessctl(&["list-ports".to_string()])?;
    if !out.status.success() {
        bail!(
            "rootlessctl list-ports failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(parse_list_ports(&String::from_utf8_lossy(&out.stdout)))
}

/// Parse `rootlessctl list-ports` table output.
///
/// Columns: ID PROTO PARENTIP PARENTPORT CHILDIP CHILDPORT. CHILDIP is commonly
/// blank, so fields are taken from the ends rather than by fixed index — a
/// blank middle column would otherwise shift everything after it.
pub fn parse_list_ports(stdout: &str) -> Vec<LiveForward> {
    stdout
        .lines()
        .skip(1) // header
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 4 {
                return None;
            }
            Some(LiveForward {
                id: f[0].parse().ok()?,
                host_ip: f[2].to_string(),
                host_port: f[3].parse().ok()?,
                // CHILDPORT is last; CHILDIP may be absent entirely.
                container_port: f[f.len() - 1].parse().ok()?,
            })
        })
        .collect()
}

/// Add one forward. Returns its rootlesskit ID.
pub fn add_live(port: &PortForward) -> std::result::Result<u32, AddFailure> {
    let out = match rootlessctl(&["add-ports".to_string(), port.spec()]) {
        Ok(out) => out,
        Err(e) => {
            return Err(AddFailure::Other {
                detail: format!("{e:#}"),
            })
        }
    };
    if !out.status.success() {
        return Err(classify_add_error(&String::from_utf8_lossy(&out.stderr)));
    }
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .map_err(|_| AddFailure::Other {
            detail: format!(
                "rootlessctl accepted the forward but its id was unreadable: {:?}",
                String::from_utf8_lossy(&out.stdout)
            ),
        })
}

/// Remove one forward by rootlesskit ID. Absent is success: removal is
/// idempotent so a retried teardown after a partial failure does not fail.
pub fn remove_live(id: u32) -> Result<()> {
    let out = rootlessctl(&["remove-ports".to_string(), id.to_string()])?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    if stderr.contains("not found") || stderr.contains("no such") {
        return Ok(());
    }
    bail!("rootlessctl remove-ports {id} failed: {}", stderr.trim());
}

// --- ownership, for safe reclamation (F-97) -----------------------------------

/// One project's claim on a forward, as reconcile sees it.
pub struct Declaration {
    pub project: String,
    pub port: PortForward,
    pub running: bool,
}

/// What reconcile may do with a live forward.
#[derive(Debug, PartialEq, Eq)]
pub enum Disposition {
    /// Exactly matches a running project's declaration: this is the correct
    /// state, not an orphan. Leave it, say nothing.
    Correct,
    /// Exactly matches a *stopped* project's declaration. Ours by construction
    /// — stop withdraws forwards, so a live one here is our own leftover — and
    /// therefore safe to reclaim.
    Reclaim { project: String },
    /// Matches no declaration at all. We cannot prove we created it, so we must
    /// not remove it: rootlesskit is shared with whatever else the user runs,
    /// and a forward they added themselves is indistinguishable from one of our
    /// orphans. Leave it and report it (F-97).
    Unattributable,
}

/// Decide what reconcile may do with one live forward.
///
/// The rule is ownership by *exact* declaration match — host address, host port
/// and container port together, not host port alone. Before F-97 the sweep
/// removed every forward no project declared, which silently destroyed the
/// user's own `rootlessctl` forwards: a cleanup command deleting configuration
/// it never created, on the assumption that anything unfamiliar was its own
/// litter. Cleanup verifies before destroying.
pub fn classify_forward(f: &LiveForward, declarations: &[Declaration]) -> Disposition {
    for d in declarations {
        let matches = d.port.host_ip == f.host_ip
            && d.port.host_port == f.host_port
            && d.port.container_port == f.container_port;
        if matches {
            return if d.running {
                Disposition::Correct
            } else {
                Disposition::Reclaim {
                    project: d.project.clone(),
                }
            };
        }
    }
    Disposition::Unattributable
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_port_maps_to_itself_on_loopback() {
        let p = parse_port_arg("8000", false).unwrap();
        assert_eq!(p.host_ip, LOOPBACK);
        assert_eq!(p.host_port, 8000);
        assert_eq!(p.container_port, 8000);
        assert_eq!(p.spec(), "127.0.0.1:8000:8000/tcp");
        assert_eq!(p.url(), "http://127.0.0.1:8000");
    }

    #[test]
    fn host_and_container_ports_can_differ() {
        let p = parse_port_arg("9000:8000", false).unwrap();
        assert_eq!(p.host_port, 9000);
        assert_eq!(p.container_port, 8000);
    }

    /// Loopback unless asked otherwise: a dev server must not land on the LAN
    /// because someone forwarded a port.
    #[test]
    fn the_default_bind_is_loopback_and_expose_opts_out() {
        assert_eq!(parse_port_arg("8000", false).unwrap().host_ip, LOOPBACK);
        assert!(!parse_port_arg("8000", false).unwrap().exposed());
        let e = parse_port_arg("8000", true).unwrap();
        assert_eq!(e.host_ip, ALL_INTERFACES);
        assert!(e.exposed());
    }

    /// An address written in the spec beats the flag, so what is written is
    /// never silently overridden by a flag the reader cannot see.
    #[test]
    fn an_explicit_address_wins_over_the_expose_flag() {
        let p = parse_port_arg("127.0.0.1:9000:8000", true).unwrap();
        assert_eq!(p.host_ip, LOOPBACK, "the written address must win");
    }

    /// 0.0.0.0 is where to bind, not where to point a browser.
    #[test]
    fn an_exposed_forwards_url_is_loopback_not_the_wildcard() {
        let p = parse_port_arg("8000", true).unwrap();
        assert_eq!(p.url(), "http://127.0.0.1:8000");
    }

    #[test]
    fn nonsense_ports_are_refused_with_the_reason() {
        for bad in ["", "abc", "0", "70000", "1:2:3:4"] {
            assert!(
                parse_port_arg(bad, false).is_err(),
                "{bad:?} should be refused"
            );
        }
        let msg = format!("{:#}", parse_port_arg("70000", false).unwrap_err());
        assert!(msg.contains("65535"), "the limit should be named: {msg}");
    }

    #[test]
    fn the_label_round_trips() {
        let ports = vec![
            parse_port_arg("8000", false).unwrap(),
            parse_port_arg("0.0.0.0:9000:3000", false).unwrap(),
        ];
        let encoded = encode_label(&ports);
        assert_eq!(decode_label(&encoded), ports);
    }

    /// A label from a newer version must not make `list` or `stop` unusable.
    #[test]
    fn an_unreadable_label_entry_is_skipped_not_fatal() {
        let decoded = decode_label("127.0.0.1:8000:8000/tcp,garbage,,9000:3000");
        assert_eq!(
            decoded.len(),
            2,
            "the good entries must survive: {decoded:?}"
        );
        assert_eq!(decoded[0].host_port, 8000);
        assert_eq!(decoded[1].host_port, 9000);
    }

    /// CHILDIP is commonly blank in rootlessctl's table. Indexing from the left
    /// would read CHILDPORT out of the wrong column when it is.
    #[test]
    fn list_ports_parses_a_blank_child_ip_column() {
        let stdout = "\
ID    PROTO    PARENTIP     PARENTPORT    CHILDIP    CHILDPORT
1     tcp      127.0.0.1    8000                     8000
2     tcp      0.0.0.0      9000                     3000
";
        let live = parse_list_ports(stdout);
        assert_eq!(live.len(), 2);
        assert_eq!(live[0].id, 1);
        assert_eq!(live[0].host_port, 8000);
        assert_eq!(
            live[0].container_port, 8000,
            "container port must come from the last column, not a fixed index"
        );
        assert_eq!(live[1].host_ip, "0.0.0.0");
        assert_eq!(live[1].container_port, 3000);
    }

    #[test]
    fn an_empty_port_table_yields_nothing() {
        assert!(parse_list_ports("ID PROTO PARENTIP PARENTPORT CHILDIP CHILDPORT\n").is_empty());
        assert!(parse_list_ports("").is_empty());
    }

    /// The two collisions need different remedies, so they must stay apart all
    /// the way to the user. These are rootlesskit's real message shapes,
    /// captured from live runs.
    #[test]
    fn the_two_collision_kinds_are_told_apart() {
        assert!(matches!(
            classify_add_error("error: conflict with ID 1"),
            AddFailure::HeldByAForward { .. }
        ));
        assert!(matches!(
            classify_add_error("error: listen tcp 127.0.0.1:5433: bind: address already in use"),
            AddFailure::HeldByTheHost { .. }
        ));
        assert!(matches!(
            classify_add_error("error: connection refused"),
            AddFailure::Other { .. }
        ));
    }

    fn decl(project: &str, spec: &str, running: bool) -> Declaration {
        Declaration {
            project: project.into(),
            port: parse_port_arg(spec, false).unwrap(),
            running,
        }
    }
    fn live(id: u32, host_ip: &str, host_port: u16, container_port: u16) -> LiveForward {
        LiveForward {
            id,
            host_ip: host_ip.into(),
            host_port,
            container_port,
        }
    }

    /// THE F-98 GUARD. An unvalidated bind address let `:9000:8000` through as
    /// an empty host_ip, which rootlesskit binds as `*:9000` — EVERY interface
    /// — with no --expose and no warning. Verified on this host before the fix.
    #[test]
    fn an_empty_or_bogus_bind_address_is_refused() {
        for bad in [
            ":9000:8000",
            "localhost:9000:8000",
            "0:9000:8000",
            "nope:1:2",
        ] {
            let r = parse_port_arg(bad, false);
            assert!(
                r.is_err(),
                "{bad:?} must be refused: an unvalidated address binds interfaces \
                 the user never asked for, and silently"
            );
        }
        let msg = format!("{:#}", parse_port_arg(":9000:8000", false).unwrap_err());
        assert!(
            msg.contains("127.0.0.1") && msg.contains("0.0.0.0"),
            "the refusal must name the accepted forms: {msg}"
        );
    }

    /// Exposure is "not loopback", not "equals 0.0.0.0". A real NIC address is
    /// LAN-reachable and must say so — before F-98 it reported itself private.
    #[test]
    fn a_specific_nic_address_counts_as_exposed() {
        let lan = parse_port_arg("10.0.2.15:9000:8000", false).unwrap();
        assert!(
            lan.exposed(),
            "a bind to a routable NIC address is reachable off this machine"
        );
        let wild = parse_port_arg("0.0.0.0:9000:8000", false).unwrap();
        assert!(wild.exposed());
        let local = parse_port_arg("127.0.0.1:9000:8000", false).unwrap();
        assert!(!local.exposed(), "loopback is not exposed");
        // Any loopback address, not just the canonical one.
        let local2 = parse_port_arg("127.0.0.5:9000:8000", false).unwrap();
        assert!(!local2.exposed(), "127.0.0.0/8 is all loopback");
    }

    /// Leading-zero octets are REFUSED, not silently reinterpreted.
    ///
    /// `127.000.000.001` is ambiguous — historically octal — and accepting it
    /// is a well-worn way to smuggle one address past a check that reads
    /// another. Rust's parser refuses it and we keep that refusal rather than
    /// normalising it into something the user did not write.
    #[test]
    fn ambiguous_leading_zero_addresses_are_refused() {
        assert!(parse_port_arg("127.000.000.001:9000:8000", false).is_err());
    }

    /// Addresses are normalised to one spelling, so the label, rootlesskit and
    /// the live-forward match cannot disagree over cosmetics.
    #[test]
    fn bind_addresses_are_normalised() {
        let p = parse_port_arg("0:0:0:0:0:0:0:1:9000:8000", false);
        // An IPv6 literal without brackets is genuinely ambiguous against the
        // colon-separated spec, so it is refused rather than guessed at.
        assert!(
            p.is_err(),
            "an unbracketed IPv6 literal cannot be disambiguated"
        );
        // The forms we do accept round-trip unchanged.
        for good in ["127.0.0.1", "0.0.0.0", "10.0.2.15"] {
            let p = parse_port_arg(&format!("{good}:9000:8000"), false).unwrap();
            assert_eq!(p.host_ip, good);
        }
    }

    /// THE F-97 GUARD. A forward nemr did not create must never be removed by a
    /// cleanup command. The user's own rootlessctl forwards are
    /// indistinguishable from our orphans, so the only safe rule is: no
    /// declaration, no deletion.
    #[test]
    fn a_forward_no_project_declares_is_left_alone_not_deleted() {
        let decls = [decl("web", "127.0.0.1:8000:8000", true)];
        let theirs = live(7, "127.0.0.1", 5433, 5432); // a user's own forward
        assert_eq!(
            classify_forward(&theirs, &decls),
            Disposition::Unattributable,
            "a forward we cannot prove we created must be left alone"
        );
    }

    #[test]
    fn a_running_projects_forward_is_correct_not_an_orphan() {
        let decls = [decl("web", "127.0.0.1:8000:8000", true)];
        assert_eq!(
            classify_forward(&live(1, "127.0.0.1", 8000, 8000), &decls),
            Disposition::Correct
        );
    }

    /// A stopped project should have no live forward — stop withdraws them — so
    /// one that exists is our own leftover and is safe to reclaim.
    #[test]
    fn a_stopped_projects_leftover_forward_is_reclaimed() {
        let decls = [decl("web", "127.0.0.1:8000:8000", false)];
        assert_eq!(
            classify_forward(&live(1, "127.0.0.1", 8000, 8000), &decls),
            Disposition::Reclaim {
                project: "web".into()
            }
        );
    }

    /// Ownership is the WHOLE tuple. Matching on host port alone would claim a
    /// forward that merely shares a port number but goes somewhere else.
    #[test]
    fn ownership_requires_the_whole_tuple_not_just_the_host_port() {
        let decls = [decl("web", "127.0.0.1:8000:8000", false)];
        // Same host port, different container port — not the one we declared.
        assert_eq!(
            classify_forward(&live(1, "127.0.0.1", 8000, 9999), &decls),
            Disposition::Unattributable,
            "a different container port is a different forward"
        );
        // Same ports, different bind address.
        assert_eq!(
            classify_forward(&live(1, "0.0.0.0", 8000, 8000), &decls),
            Disposition::Unattributable,
            "a different bind address is a different forward"
        );
    }

    /// Whatever the classification, the operator's own words survive — a
    /// category with the detail thrown away is the F-66 shape.
    #[test]
    fn a_classified_failure_still_carries_the_original_message() {
        let f =
            classify_add_error("error: listen tcp 127.0.0.1:5433: bind: address already in use");
        assert!(
            f.to_string().contains("5433"),
            "the underlying message must survive classification: {f}"
        );
    }
}
