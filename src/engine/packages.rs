//! Declared packages: what a session's owner installed, carried in the bundle
//! so an imported session can be provisioned to match (F-118).
//!
//! # What the list means — intent, not closure
//!
//! The list records what was ASKED FOR (`apt-get install jq` → `jq`), never the
//! transitive set apt resolved it to (`libjq1`, `libonig5`). Product Owner
//! ruling, with the deciding argument: a bundle pinning the dependency closure
//! is wrong on any destination whose apt resolves differently — a graph we do
//! not control, computed on a machine that is not the one installing. The
//! destination's resolution may therefore legitimately differ from the
//! source's: different transitive set, different versions. That is a property
//! of the design, not a defect.
//!
//! # Why detection reads the snapshot, not the session
//!
//! Packages land on the container's writable snapshot layer, which survives
//! stop/start (F-118) and is readable from the host while the project is
//! STOPPED — which export already requires. Measured at 0.01–0.04s: no exec,
//! no running task, no containerd write. The upperdir's dpkg status is the
//! container's truth; the FIRST lowerdir in overlay order that has the file is
//! the base's. "First that has it", not "first": the base status sat three
//! layers deep on the machine this was built on, and picking the wrong layer
//! reports the entire base as user-installed, on every project, for ever.

use std::collections::BTreeSet;

use anyhow::{Context, Result};

/// The declared-packages file, as it travels in `.nemr-state/`.
///
/// Sorted and timestamp-free BY CONSTRUCTION (`BTreeSet`), because export is
/// deterministic — two exports of unchanged content must produce identical
/// bundles — and the existing determinism guard would stay green against a
/// timestamped or unordered file written into the tree before the walk.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DeclaredPackages {
    /// Format version, so a future shape can be refused rather than misread.
    pub version: u32,
    /// Package names, sorted, deduplicated. Names only — see the module doc
    /// for why versions and dependencies deliberately do not travel.
    pub packages: BTreeSet<String>,
}

impl DeclaredPackages {
    pub const VERSION: u32 = 1;
    /// Where the file lives on the volume, relative to the mount point. Inside
    /// `.nemr-state/`, so it travels by the same policy as the session state.
    pub const VOLUME_PATH: &'static str = ".nemr-state/packages.json";

    pub fn new(packages: BTreeSet<String>) -> Self {
        Self {
            version: Self::VERSION,
            packages,
        }
    }
}

/// Package names marked `install ok installed` in a dpkg status file.
///
/// A parser over the two lines this decision needs, not a dpkg model: `Package:`
/// names the entry, `Status:` says whether it is really installed. Entries in
/// other states (`deinstall ok config-files`, half-installed) are excluded —
/// a removed-but-not-purged package is not something the owner has.
pub fn installed_packages(dpkg_status: &str) -> BTreeSet<String> {
    let mut packages = BTreeSet::new();
    let mut current: Option<&str> = None;
    for line in dpkg_status.lines() {
        if let Some(name) = line.strip_prefix("Package: ") {
            current = Some(name.trim());
        } else if let Some(status) = line.strip_prefix("Status: ") {
            if status.trim().ends_with("install ok installed") {
                if let Some(name) = current {
                    packages.insert(name.to_string());
                }
            }
        } else if line.is_empty() {
            current = None;
        }
    }
    packages
}

/// Package names marked `Auto-Installed: 1` in apt's extended_states file.
///
/// These are the packages apt pulled in as dependencies — the closure, not the
/// intent. Absent file means no marks, which is correct: a container where apt
/// never ran has no auto-installed packages.
pub fn auto_installed(extended_states: &str) -> BTreeSet<String> {
    let mut auto = BTreeSet::new();
    let mut current: Option<&str> = None;
    for line in extended_states.lines() {
        if let Some(name) = line.strip_prefix("Package: ") {
            current = Some(name.trim());
        } else if line.trim() == "Auto-Installed: 1" {
            if let Some(name) = current {
                auto.insert(name.to_string());
            }
        } else if line.is_empty() {
            current = None;
        }
    }
    auto
}

/// THE ALGORITHM: what the owner declared, from the container's files and the
/// base's.
///
/// (installed in container − installed in base) ∩ manual. Pure, so the whole
/// decision is unit-testable without a host; the fixture in the tests is the
/// measured output of a real project with `jq` installed.
pub fn declared(
    container_status: &str,
    base_status: &str,
    container_extended_states: &str,
) -> BTreeSet<String> {
    let container = installed_packages(container_status);
    let base = installed_packages(base_status);
    let auto = auto_installed(container_extended_states);
    container
        .difference(&base)
        .filter(|p| !auto.contains(*p))
        .cloned()
        .collect()
}

/// Serialise for the volume: sorted keys, trailing newline, no timestamps —
/// byte-stable for identical content, which bundle determinism requires.
pub fn to_json(list: &DeclaredPackages) -> Result<String> {
    let mut s = serde_json::to_string_pretty(list).context("serialising the package list")?;
    s.push('\n');
    Ok(s)
}

pub fn from_json(raw: &str) -> Result<DeclaredPackages> {
    let list: DeclaredPackages =
        serde_json::from_str(raw).context("parsing .nemr-state/packages.json")?;
    if list.version > DeclaredPackages::VERSION {
        anyhow::bail!(
            "packages.json is version {} but this engine understands up to {}. \
             It was written by a newer nemr; upgrade this one.",
            list.version,
            DeclaredPackages::VERSION
        );
    }
    Ok(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The measured shape from a real project (detect-345897, 2026-08-29):
    /// `apt-get install jq` produced three new packages, of which apt marked
    /// two auto-installed. The declaration is the one that was asked for.
    fn base_status() -> String {
        ["ca-certificates", "git", "less", "libonig-base"]
            .iter()
            .map(|p| format!("Package: {p}\nStatus: install ok installed\n\n"))
            .collect()
    }

    fn container_status() -> String {
        [
            "ca-certificates",
            "git",
            "jq",
            "less",
            "libjq1",
            "libonig-base",
            "libonig5",
        ]
        .iter()
        .map(|p| format!("Package: {p}\nStatus: install ok installed\n\n"))
        .collect()
    }

    const EXTENDED_STATES: &str = "Package: libjq1\nArchitecture: amd64\nAuto-Installed: 1\n\n\
                                   Package: libonig5\nArchitecture: amd64\nAuto-Installed: 1\n\n";

    /// Intent, not closure: `jq` alone, never its dependencies.
    #[test]
    fn the_declaration_is_what_was_asked_for_not_what_apt_resolved() {
        let got = declared(&container_status(), &base_status(), EXTENDED_STATES);
        assert_eq!(
            got.iter().collect::<Vec<_>>(),
            ["jq"],
            "libjq1 and libonig5 are apt's business, not the declaration's"
        );
    }

    /// A base package the user did not touch must never be declared — the
    /// failure mode of picking the wrong lowerdir is the whole base appearing
    /// new, so this is the assertion that would catch it at the unit level.
    #[test]
    fn base_packages_are_never_declared() {
        let got = declared(&container_status(), &base_status(), EXTENDED_STATES);
        for p in ["ca-certificates", "git", "less", "libonig-base"] {
            assert!(
                !got.contains(p),
                "{p} is in the base and must not be declared"
            );
        }
    }

    /// An empty diff is an empty declaration, not an error: most projects
    /// install nothing.
    #[test]
    fn an_unmodified_container_declares_nothing() {
        let got = declared(&base_status(), &base_status(), "");
        assert!(got.is_empty());
    }

    /// A removed-but-not-purged package is not installed. dpkg keeps the entry
    /// with `deinstall ok config-files`, and declaring it would make provision
    /// install something the owner deliberately removed.
    #[test]
    fn a_removed_package_is_not_declared() {
        let container = format!(
            "{}Package: jq\nStatus: deinstall ok config-files\n\n",
            base_status()
        );
        let got = declared(&container, &base_status(), "");
        assert!(!got.contains("jq"));
    }

    /// Missing extended_states means no auto marks — everything new is manual.
    /// (A container where apt never wrote the file, or a package installed with
    /// raw dpkg.)
    #[test]
    fn no_extended_states_means_everything_new_is_declared() {
        let got = declared(&container_status(), &base_status(), "");
        assert_eq!(
            got.iter().collect::<Vec<_>>(),
            ["jq", "libjq1", "libonig5"],
            "with no auto marks there is nothing to subtract"
        );
    }

    /// Byte-stable serialisation: same content, same bytes, no timestamps.
    /// Bundle determinism rests on this — the existing determinism guard would
    /// stay green against a timestamped file, so the property is pinned here,
    /// at the source.
    #[test]
    fn serialisation_is_byte_stable_and_sorted() {
        let a = DeclaredPackages::new(
            ["zsh", "jq", "ripgrep"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        );
        let b = DeclaredPackages::new(
            ["ripgrep", "zsh", "jq"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        );
        let ja = to_json(&a).unwrap();
        let jb = to_json(&b).unwrap();
        assert_eq!(ja, jb, "insertion order must not leak into the bytes");
        assert!(!ja.contains("time"), "no timestamps");
        let zi = ja.find("zsh").unwrap();
        let ji = ja.find("jq").unwrap();
        assert!(ji < zi, "sorted output");
    }

    /// A newer format is refused with advice, not misread.
    #[test]
    fn a_future_version_is_refused() {
        let err = from_json(r#"{"version": 99, "packages": []}"#).unwrap_err();
        assert!(format!("{err:#}").contains("newer nemr"));
    }

    /// Round trip.
    #[test]
    fn json_round_trips() {
        let list = DeclaredPackages::new(["jq"].iter().map(|s| s.to_string()).collect());
        let back = from_json(&to_json(&list).unwrap()).unwrap();
        assert_eq!(back, list);
    }
}

// --- reading the snapshot from the host -------------------------------------

/// The three files the detector needs, read from a STOPPED project's snapshot.
///
/// The overlay paths live in rootlesskit's mount namespace, so every read goes
/// through `nsenter` into it — the same hop `netns.rs` makes for the network
/// namespace, for the same reason: the daemon runs on the host and the state
/// does not. Reading the paths directly from the host is the classic wrong-place
/// negative (loop.md's first rule): "no such file" there means nothing.
pub struct SnapshotPackageFiles {
    pub container_status: String,
    pub base_status: String,
    pub container_extended_states: String,
}

/// Read one file out of rootlesskit's mount namespace. Ok(None) = genuinely
/// absent THERE (checked, not assumed); Err = could not look.
fn read_in_rootlesskit(pid: &str, path: &str) -> Result<Option<String>> {
    let out = std::process::Command::new("nsenter")
        .args(["-t", pid, "-U", "-m", "--preserve-credentials", "--"])
        .args(["sh", "-c"])
        .arg(format!(
            "if [ -f '{path}' ]; then cat '{path}'; else echo NEMR_ABSENT >&2; exit 3; fi"
        ))
        .output()
        .context("entering rootlesskit's mount namespace")?;
    if out.status.code() == Some(3) && String::from_utf8_lossy(&out.stderr).contains("NEMR_ABSENT")
    {
        return Ok(None);
    }
    if !out.status.success() {
        anyhow::bail!(
            "reading {path} in rootlesskit's mount namespace failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(Some(String::from_utf8_lossy(&out.stdout).into_owned()))
}

/// Locate and read the detector's inputs for a snapshot.
///
/// `lowers` must be in overlay order. The base status is the FIRST lowerdir
/// that HAS the file — measured on this machine it sat three layers deep, and
/// taking layer zero would report the entire base as user-installed. A missing
/// base status in EVERY layer is an error, not an empty set: it means we are
/// not looking at an image with dpkg at all, and diffing against nothing would
/// declare the whole container.
pub fn read_snapshot_package_files(
    rootlesskit_pid: &str,
    upper: &str,
    lowers: &[String],
) -> Result<SnapshotPackageFiles> {
    const STATUS: &str = "var/lib/dpkg/status";
    const EXTENDED: &str = "var/lib/apt/extended_states";

    // The container's status: the upperdir copy when dpkg ran in the session
    // (overlayfs copies-up on write), else the base's — an untouched container
    // IS the base, and the diff is correctly empty.
    let container_status = read_in_rootlesskit(rootlesskit_pid, &format!("{upper}/{STATUS}"))?;

    let mut base_status = None;
    for lower in lowers {
        if let Some(found) = read_in_rootlesskit(rootlesskit_pid, &format!("{lower}/{STATUS}"))? {
            base_status = Some(found);
            break;
        }
    }
    let base_status = base_status.with_context(|| {
        format!(
            "no dpkg status in any of the {} lower layers — this snapshot is not built on \\
             an image that has dpkg, so a package diff would be meaningless",
            lowers.len()
        )
    })?;

    let container_status = match container_status {
        Some(s) => s,
        None => base_status.clone(),
    };

    let container_extended_states =
        read_in_rootlesskit(rootlesskit_pid, &format!("{upper}/{EXTENDED}"))?.unwrap_or_default();

    Ok(SnapshotPackageFiles {
        container_status,
        base_status,
        container_extended_states,
    })
}

// --- provisioning ------------------------------------------------------------

/// The result of provisioning, verified by OUTCOME rather than exit status.
#[derive(Debug)]
pub struct ProvisionReport {
    /// Packages that are now present (verified individually).
    pub installed: Vec<String>,
    /// Packages that could not be installed, with apt's own words.
    pub failed: Vec<(String, String)>,
}

/// The declared list for a project, read from its volume. Ok(None) = no file,
/// which is the common case and means "nothing to provision".
pub fn read_declared(mount_point: &std::path::Path) -> Result<Option<DeclaredPackages>> {
    let path = mount_point.join(DeclaredPackages::VOLUME_PATH);
    match std::fs::read_to_string(&path) {
        Ok(raw) => Ok(Some(from_json(&raw)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// The shell script provision runs inside the session, for one package.
///
/// F-122: `apt-get update` exits 0 when EVERY repository is unreachable
/// (measured; only `APT::Update::Error-Mode=any` changes that). So no exit
/// status in this pipeline is trusted for the thing that matters. The script's
/// last line is a VERDICT read from the system itself: dpkg-query for the
/// installed state, which is the outcome, not the report of the tool that was
/// supposed to produce it.
///
/// `dpkg --audit` afterwards is the half-install control: a package left `iU`
/// is invisible now and misattributes the NEXT failure, so it is checked
/// explicitly rather than inferred from apt's status.
pub fn provision_script(package: &str) -> String {
    format!(
        r#"export DEBIAN_FRONTEND=noninteractive
apt-get -o APT::Update::Error-Mode=any update >/tmp/nemr-apt.log 2>&1 \
  || {{ echo "NEMR_VERDICT:UPDATE_FAILED"; tail -5 /tmp/nemr-apt.log; exit 0; }}
apt-get install -y --no-install-recommends '{package}' >>/tmp/nemr-apt.log 2>&1
# The verdict comes from dpkg, not from apt's exit status.
if dpkg-query -W -f '${{Status}}' '{package}' 2>/dev/null | grep -q 'install ok installed'; then
  if [ -n "$(dpkg --audit 2>/dev/null)" ]; then
    echo "NEMR_VERDICT:AUDIT_DIRTY"; dpkg --audit 2>/dev/null | head -5
  else
    echo "NEMR_VERDICT:INSTALLED"
  fi
else
  echo "NEMR_VERDICT:NOT_INSTALLED"; tail -5 /tmp/nemr-apt.log
fi
exit 0
"#
    )
}

/// Interpret one provision run's output. Pure, so the verdict logic is
/// unit-tested against the exact strings the script emits.
pub fn parse_verdict(output: &str) -> (bool, String) {
    for line in output.lines() {
        if let Some(v) = line.trim().strip_prefix("NEMR_VERDICT:") {
            return match v {
                "INSTALLED" => (true, String::new()),
                "UPDATE_FAILED" => (
                    false,
                    format!(
                        "the package index could not be refreshed — check this session's \
                         network. Detail:\n{}",
                        tail_after_verdict(output)
                    ),
                ),
                "AUDIT_DIRTY" => (
                    false,
                    format!(
                        "installed, but dpkg reports a package left half-configured — the \
                         database must not be left inconsistent (it would misattribute the \
                         next failure). Detail:\n{}",
                        tail_after_verdict(output)
                    ),
                ),
                _ => (
                    false,
                    format!(
                        "apt could not install it. Detail:\n{}",
                        tail_after_verdict(output)
                    ),
                ),
            };
        }
    }
    (
        false,
        "the provision script produced no verdict — the session may have died mid-install"
            .to_string(),
    )
}

fn tail_after_verdict(output: &str) -> String {
    output
        .lines()
        .skip_while(|l| !l.trim().starts_with("NEMR_VERDICT:"))
        .skip(1)
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod provision_tests {
    use super::*;

    #[test]
    fn an_installed_verdict_is_success() {
        let (ok, _) = parse_verdict("NEMR_VERDICT:INSTALLED\n");
        assert!(ok);
    }

    /// F-122's core: apt-get update exiting 0 with every repository down must
    /// not read as success. The script forces Error-Mode=any and the verdict
    /// names the failure class.
    #[test]
    fn an_unreachable_index_is_failure_with_the_reason() {
        let (ok, why) = parse_verdict("NEMR_VERDICT:UPDATE_FAILED\nW: Failed to fetch http://x\n");
        assert!(!ok);
        assert!(why.contains("index could not be refreshed"), "{why}");
        assert!(why.contains("Failed to fetch"), "the detail travels: {why}");
    }

    /// A half-configured database is a failure even when the target package
    /// installed — the iU entry would misattribute the next failure.
    #[test]
    fn a_dirty_dpkg_audit_is_failure() {
        let (ok, why) = parse_verdict("NEMR_VERDICT:AUDIT_DIRTY\nThe following packages...\n");
        assert!(!ok);
        assert!(why.contains("half-configured"), "{why}");
    }

    /// No verdict at all is a failure with its own explanation, never a silent
    /// success — a session dying mid-install must not read as installed.
    #[test]
    fn a_missing_verdict_is_failure_not_silence() {
        let (ok, why) = parse_verdict("some unrelated output\n");
        assert!(!ok);
        assert!(why.contains("no verdict"), "{why}");
    }

    /// The script asks apt for the update to FAIL loudly (Error-Mode=any) and
    /// derives the final verdict from dpkg-query, not from apt's exit status.
    #[test]
    fn the_script_verifies_outcome_not_status() {
        let s = provision_script("jq");
        assert!(
            s.contains("APT::Update::Error-Mode=any"),
            "F-122: the exit-0 default is overridden"
        );
        assert!(
            s.contains("dpkg-query"),
            "the verdict is the system's state, not apt's report"
        );
        assert!(
            s.contains("dpkg --audit"),
            "the half-install control is present"
        );
    }
}
