//! The daemon must live in the host's user namespace (F-9).
//!
//! Measured 2026-09-08: a test ran the CLI under `unshare -rmn` while no
//! daemon was answering; the CLI autostarted `nemrd` *inside that namespace*,
//! the daemon bound the well-known socket (the mount namespace is a copy, so
//! the path is the host's), and it outlived the test as an orphan. Every later
//! caller on the machine — the browser acceptance's `nemr create` first — was
//! served by a daemon whose `sudo` could not load (`/etc/sudo.conf is owned by
//! uid 65534`), so every privileged call failed, and nothing named the cause:
//! `ss` cannot even see a listener in another network namespace.
//!
//! The daemon is the single writer to containerd and the only caller of the
//! privileged helper (E-09, VOL-03); a daemon that cannot elevate is not a
//! daemon. So the rule is a read at two points, refusing rather than
//! repairing: `nemrd` refuses to start unless its uid map is the host's, and
//! the CLI refuses to autostart one from anywhere else, naming the map it saw.

use anyhow::{bail, Context, Result};

/// The initial user namespace maps every uid to itself. Anything else is a
/// namespace some process made, and the privileged helper cannot run there.
pub fn uid_map_is_the_hosts(uid_map: &str) -> bool {
    let fields: Vec<&str> = uid_map.split_whitespace().collect();
    fields == ["0", "0", "4294967295"]
}

/// Refuse unless this process is in the host's user namespace.
pub fn refuse_foreign_user_namespace(who: &str) -> Result<()> {
    let map = std::fs::read_to_string("/proc/self/uid_map")
        .context("reading /proc/self/uid_map to check the user namespace")?;
    if uid_map_is_the_hosts(&map) {
        return Ok(());
    }
    bail!(
        "{who} is inside a user namespace (/proc/self/uid_map is `{}`, not the host's \
         `0 0 4294967295`). The daemon runs the privileged helper through sudo, which \
         cannot work from a namespace, and a daemon started here would serve every later \
         caller on this machine with failing mounts. Start the daemon from a plain host \
         shell (any `nemr` command there autostarts it) and retry.",
        map.split_whitespace().collect::<Vec<_>>().join(" ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hosts_map_and_nothing_else() {
        assert!(uid_map_is_the_hosts("         0          0 4294967295\n"));
        // `unshare -r`: one uid mapped, the shape measured on the orphan.
        assert!(!uid_map_is_the_hosts("         0       1000          1\n"));
        // A container's map: a range, but not the whole one.
        assert!(!uid_map_is_the_hosts("0 100000 65536\n"));
        assert!(!uid_map_is_the_hosts(""));
    }
}
