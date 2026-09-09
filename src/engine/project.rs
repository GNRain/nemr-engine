//! Project lifecycle: create (Milestone 4); start/stop/attach and list/delete
//! follow in Milestones 5 and 6.
//!
//! A project is the user-facing unit of isolation (Section 2): one named,
//! quota-bounded volume paired with one container built from the base image.

use std::collections::HashMap;

use crate::engine::agent::Agent;
use anyhow::{bail, Context, Result};

use crate::auth;
use crate::config;
use crate::containerd::client::ContainerdClient;
use crate::containerd::containers::{BindMount, ContainerSpec, StopOutcome};
use crate::engine::volume::{HelperOps, PrivilegedOps, Volume, VolumePaths, VolumeSize};
use std::os::unix::ffi::OsStringExt;
use std::path::Path;

/// Runs in the container's shell before every prompt, so a prompt never lands
/// on top of the previous command's output.
///
/// # The problem
///
/// A full-screen program interrupted with Ctrl+C — Claude Code being the case
/// that matters here — exits with the cursor wherever it happened to be
/// rendering, and often with the cursor hidden and colours still set. Bash then
/// draws its next prompt at that position, so the prompt and everything typed
/// afterwards overwrites the program's output. Recovering means running
/// `clear`, which is a poor thing to ask of every user after every session.
///
/// # The fix
///
/// Three parts, all emitted before each prompt:
///
/// 1. Show the cursor and reset attributes, undoing what the interrupted
///    program left set.
/// 2. Move to a fresh line, but *only* when the previous output ended
///    mid-line. Printing exactly `$COLUMNS` spaces from column `c` lands the
///    cursor at column `c` of the next line when `c > 0`, and — thanks to
///    deferred wrap — leaves it on the current line when `c == 0`. The
///    following `\r` returns to column 0. So a command that ended cleanly gets
///    no blank line, and one that ended mid-line gets exactly one. This is the
///    partial-line trick zsh uses for `PROMPT_SP`, with spaces instead of
///    zsh's inverse `%` marker so nothing visible is left behind.
/// 3. Erase from the cursor to the end of the screen (`\e[J`).
///
/// Step 3 is what handles the interrupted-TUI case, and step 2 alone does not.
/// An Ink-based program like Claude Code re-renders by moving the cursor *up*
/// over its own frame; interrupted mid-frame, it leaves the cursor above output
/// that is still on screen, at column 0. Bash then draws its prompt there, and
/// the prompt — plus everything typed after it — overwrites the stale frame
/// line by line. Measured against a terminal emulator, the prompt landed on top
/// of "claude output line 7" and `logout` on top of line 8.
///
/// Erasing below the cursor is safe at prompt time because a well-behaved
/// command leaves the cursor after its last line, where there is nothing to
/// erase. Anything still below is a frame nobody is managing any more, and it
/// is going to be overwritten regardless — erased is strictly better than
/// garbled. Scrollback above the cursor is untouched, so the session's history
/// remains readable.
///
/// Set through the exec's environment rather than the image, because it is a
/// property of an interactive attach session rather than of the image itself —
/// and bash reads `PROMPT_COMMAND` from the environment.
const PROMPT_TIDY: &str = concat!(
    "PROMPT_COMMAND=",
    r#"printf '\e[?25h\e[0m%*s\r\e[J' "${COLUMNS:-80}" ''"#,
);

/// Label keys written onto the container record.
///
/// Prefixed so engine metadata is distinguishable from anything else that
/// might label a container in this namespace.
pub const LABEL_PROJECT: &str = "nemr.project";
pub const LABEL_VOLUME: &str = "nemr.volume";
pub const LABEL_SIZE: &str = "nemr.size";
/// The agent a project runs (E-15). Absent on projects created before the
/// field existed — those are Claude Code by construction, so a missing label
/// reads back as the default.
pub const LABEL_AGENT: &str = "nemr.agent";
/// Declared port forwards, comma-separated rootlesskit specs. The project's
/// authoritative record; the live forward set is derived from it (WP-M).
pub const LABEL_PORTS: &str = "nemr.ports";
/// The session's network index: `10.99.<index>.0/24` (NET-02). Recorded so the
/// same addresses are reapplied on every start and no two sessions collide.
pub const LABEL_NETNS: &str = "nemr.netns";

/// Labels written onto a project's container record.
///
/// Factored out of `create` so the round trip `list` and `reconcile_orphans`
/// depend on is testable without containerd: both key on these exact keys, so a
/// silent change here makes projects invisible to `list` and makes every volume
/// look like an orphan to reconciliation.
pub fn project_labels(
    name: &str,
    volume_path: &str,
    size: VolumeSize,
    agent: Agent,
    network: Option<netns::Allocation>,
) -> HashMap<String, String> {
    let mut labels = HashMap::new();
    labels.insert(LABEL_PROJECT.to_string(), name.to_string());
    labels.insert(LABEL_VOLUME.to_string(), volume_path.to_string());
    labels.insert(LABEL_SIZE.to_string(), size.to_string());
    labels.insert(LABEL_AGENT.to_string(), agent.id().to_string());
    if let Some(alloc) = network {
        labels.insert(LABEL_NETNS.to_string(), netns::encode_allocation(alloc));
    }
    labels
}

/// Create a project: a quota-bounded volume plus a ready-to-start container.
///
/// # Ordering
///
/// The sequence is chosen so that failures cost as little as possible and
/// never leave partial state:
///
/// 1. Validate the name, and reject a duplicate **before** touching anything.
/// 2. Resolve host credentials (AUTH-03). This fails fast, before a volume is
///    allocated, because the fix is on the user's side and there is no point
///    provisioning storage for a container that could not authenticate.
/// 3. Create and mount the volume (Milestone 3).
/// 4. Create the container. If this fails, the volume guard's `Drop` releases
///    the mount and loop device, and the backing file is removed.
/// 5. Only once the container exists, `persist()` the volume so it outlives
///    the guard — the container now depends on it.
///
/// Step 2 never refuses for a missing file (E-21, ruled 2026-09-08, amending
/// AUTH-03): a host with no credential gets the engine's placeholder, and a
/// restore takes exactly the same path — E-14's split is gone.
pub async fn create(
    client: &ContainerdClient,
    name: &str,
    size: VolumeSize,
    agent: Agent,
) -> Result<ProjectSummary> {
    create_with_auth(client, name, size, agent).await
}

async fn create_with_auth(
    client: &ContainerdClient,
    name: &str,
    size: VolumeSize,
    agent: Agent,
) -> Result<ProjectSummary> {
    crate::engine::volume::validate_name(name)
        .with_context(|| format!("invalid project name {name:?}"))?;

    let container_id = config::container_id(name);
    let paths = VolumePaths::from_env()?;

    // AC-4.2: a duplicate must fail clearly rather than overwrite or produce a
    // half-owned pair. Check both halves — either one existing means the name
    // is taken, and a project with only one half is a broken state we should
    // report rather than silently complete.
    // F-70: typed, so a daemon mapping ErrorKind to a gRPC status returns
    // Conflict rather than Internal. These conditions were reported as untyped
    // anyhow strings while `Error::ProjectExists` sat in the enum, reachable by
    // nothing — the taxonomy declared and never applied.
    if client.container_exists(&container_id).await? {
        return Err(crate::error::Error::ProjectExists {
            name: name.to_string(),
        }
        .into());
    }
    if paths.image_file(name).exists() {
        return Err(crate::error::Error::ProjectExists {
            name: name.to_string(),
        }
        .into());
    }

    // AUTH-01/02/03: credentials come from the host, read-only, and their
    // absence is a clear failure before anything is provisioned.
    //
    // E-14: exempt for a restore. AUTH-03 makes a missing credential fatal "at
    // creation time"; E-11 requires `nemr import` to work with no credential
    // and no network at all. Those did not collide while import needed a
    // pre-existing project — the create had already happened, with a
    // credential. Now that a restore creates its own project, they do.
    //
    // E-11's guarantee is the one that holds: a bundle restored on a fresh
    // machine before the user has logged in is the *normal* case, and import's
    // own output already ends with "Authenticate on this host, then: nemr
    // start". A credential is still required to run anything — `create` is
    // unchanged and `attach` still resolves one — so this narrows *where*
    // AUTH-03 fires, not whether it does. Raised as E-14 because AUTH-03 is a
    // Section 3 requirement and narrowing it is not mine to decide.
    // AUTH-03 as amended by E-21 (2026-09-08): a host with no credential is
    // a machine that has never logged in, not a fault. `create` and a restore
    // behave identically — the placeholder is written where the credential
    // will be, bound read-write like a real one, and `/login` inside the
    // session writes the real credential through it onto this host. A
    // credential that IS present is still held to what it says: readable,
    // and not dead (a spent refresh token or a blanked file — F-129).
    let credentials = auth::ensure_host_credential_file()?;
    let credential_dir = auth::host_credential_dir()?;
    auth::check_permissions(&credentials)?;
    auth::refuse_dead_credential(&credentials, unix_now())?;

    let volume = Volume::create(name, size, paths, HelperOps::new())
        .with_context(|| format!("failed to provision volume for project {name:?}"))?;
    let mount_point = volume.mount_point();

    // M8: create the on-volume directories that hold the relocated session state,
    // before the container binds them in. They are created on the mounted volume
    // (chowned to the invoker, so they map to root inside the container) so that
    // Claude Code's conversation history is written to the layer that travels.
    // See `session_state_mounts` and docs/state-locality.md.
    for subdir in [config::VOLUME_STATE_PROJECTS, config::VOLUME_STATE_SESSIONS] {
        let dir = mount_point.join(subdir);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create session-state dir {}", dir.display()))?;
    }

    // NET-02: allocate this session's network now, so it is recorded once and
    // reapplied verbatim at every start.
    //
    // The lock is taken here and held until the container record carries the
    // label, below: the index becomes "taken" only when it is visible to the
    // next reader, so releasing between choosing and publishing would leave the
    // window open that the lock exists to close.
    let allocation_guard = NETWORK_ALLOCATION.lock().await;
    let network = allocate_index_locked(client).await?;
    let labels = project_labels(
        name,
        &mount_point.to_string_lossy(),
        size,
        agent,
        Some(network),
    );

    let mut mounts = vec![
        // The project volume becomes the container's working directory.
        BindMount::read_write(&mount_point, config::CONTAINER_WORKDIR),
        // AUTH-02, as revised by D-02's (f) and F-14: the host's DEDICATED
        // credential DIRECTORY is bound over `/root/.claude`, read-write. A
        // directory, not the single file, because Claude Code writes the
        // credential by writing `.credentials.json.tmp.<hex>` and renaming it
        // over the target (measured 2026-09-08), which changes the inode; a
        // single-file bind cannot follow a rename, so a login's later write
        // escaped onto a container-only inode while the host kept an earlier
        // one (the human-arm failure, E-21). The directory holds ONLY the
        // credential and the `projects`/`sessions` mount points — never the
        // host's own `~/.claude`, which carries history and settings that must
        // not travel (D-02). It is a host bind and is NOT on the volume: the
        // credential never leaves the machine.
        //
        // NEMR_TEST_PRE_F14 is a test seam like NEMR_TEST_PRE_NET02 below: it
        // creates the pre-F14 single-FILE bind, the shape no fresh container
        // can otherwise have, so the directory-bind migration every existing
        // project takes at its next start is exercised end to end (F-112's
        // lesson) and the file bind's failure is provable. Production never
        // sets it.
        if std::env::var_os("NEMR_TEST_PRE_F14").is_some() {
            BindMount::read_write(&credentials, config::CONTAINER_CREDENTIALS)
        } else {
            BindMount::read_write(&credential_dir, config::CONTAINER_CLAUDE_DIR)
        },
    ];
    // M8: bind the session-critical subtrees from the volume over their rootfs
    // locations, so history and session state live on the portable layer.
    mounts.extend(session_state_mounts(&mount_point));

    let spec = ContainerSpec {
        // Test seam, the same shape as NEMR_TEST_DAEMON_PROTOCOL: creates a
        // container with the pre-NET-02 namespace list, so the upgrade path
        // every existing user takes can be exercised end to end. No freshly
        // created container can otherwise have that shape, and an untested
        // upgrade path is how a working project stops working. Production never
        // sets it.
        own_network_namespace: std::env::var_os("NEMR_TEST_PRE_NET02").is_none(),
        id: container_id.clone(),
        // Test seam, alongside NEMR_TEST_PRE_NET02 above: creates the project
        // from an OLDER published base image, which is the only way to obtain a
        // project whose rootfs differs from this engine's constant. F-116
        // recorded that as impossible; it is not, because F-85 keeps every
        // published version pullable for ever. Production never sets it.
        image: std::env::var("NEMR_TEST_BASE_IMAGE")
            .unwrap_or_else(|_| config::BASE_IMAGE.to_string()),
        mounts,
        working_dir: Some(config::CONTAINER_WORKDIR.to_string()),
        extra_env: vec![],
        args: Some(
            config::SUPERVISOR_ARGS
                .iter()
                .map(|s| s.to_string())
                .collect(),
        ),
        // Bare project name: the scope is `nemr-<name>.scope`, and passing the
        // container id (already `nemr-` prefixed) would double it.
        cgroup_name: Some(name.to_string()),
        cgroup_prefix: config::CGROUP_PREFIX.to_string(),
        labels,
    };

    if let Err(error) = client.create_container(&spec).await {
        // `volume` is still owned here, so returning drops it and releases the
        // mount and loop device. Remove the backing file too, or the duplicate
        // check above would reject a retry.
        drop(volume);
        let _ = std::fs::remove_file(VolumePaths::from_env()?.image_file(name));
        return Err(error).with_context(|| {
            format!("failed to create container for project {name:?}; volume released")
        });
    }

    // The index is now published on the record, so the next allocator can see
    // it. Only here, not a line earlier.
    drop(allocation_guard);

    // Committed: the container depends on this mount now.
    let mount_point = volume.persist();

    Ok(ProjectSummary {
        name: name.to_string(),
        container_id,
        volume_path: mount_point.to_string_lossy().to_string(),
        size,
        agent,
    })
}

/// What `adopt` produced, for the CLI to report.
#[derive(Debug, Clone)]
pub struct AdoptSummary {
    pub name: String,
    pub container_id: String,
    pub size: VolumeSize,
    pub agent: Agent,
    pub files_copied: u64,
    pub bytes_copied: u64,
    pub history_sessions: u64,
    pub history_lines_dropped: u64,
    pub source: std::path::PathBuf,
}

/// Claude Code's history key for a directory: the absolute path with every
/// `/` and `.` mapped to `-` (measured 2026-09-08). A session's project is
/// `/workspace`, so its key is `-workspace`.
pub fn history_key(abs: &Path) -> String {
    abs.to_string_lossy()
        .chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect()
}

/// A path component that is always excluded from an adopted tree: derived and
/// huge (`target/` on nemr-engine is 11G; `node_modules/` likewise), rebuilt
/// from source, and never belonging in a bundle (E-23).
fn is_always_excluded_component(name: &str) -> bool {
    name == "target" || name == "node_modules"
}

/// The files to copy from a source tree, `.gitignore`-respecting when it is a
/// git repo, with `target/` and `node_modules/` excluded unconditionally.
/// Returns the copy set as paths relative to `source`, plus whether `.git`
/// should travel whole. `.git` itself is not enumerated here (git does not list
/// its own internals); the caller copies it wholesale.
fn adopt_copy_set(source: &Path) -> Result<Vec<std::path::PathBuf>> {
    let is_git = source.join(".git").exists();
    let mut rels: Vec<std::path::PathBuf> = Vec::new();
    if is_git {
        // Tracked + untracked-but-not-ignored, NUL-separated. This is exactly
        // what `.gitignore` lets through (so `target/` — ignored — never
        // appears), and it is the measured case (a git repo).
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(source)
            .args([
                "ls-files",
                "-z",
                "--cached",
                "--others",
                "--exclude-standard",
            ])
            .output()
            .context("running git ls-files to enumerate the adoptable tree")?;
        if !out.status.success() {
            bail!(
                "git ls-files failed in {}: {}",
                source.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        for rel in out.stdout.split(|b| *b == 0) {
            if rel.is_empty() {
                continue;
            }
            let rel = std::path::PathBuf::from(std::ffi::OsString::from_vec(rel.to_vec()));
            if rel
                .components()
                .any(|c| is_always_excluded_component(&c.as_os_str().to_string_lossy()))
            {
                continue;
            }
            rels.push(rel);
        }
    } else {
        // Not a repo: copy everything except the always-excluded directories.
        for entry in walkdir_files(source)? {
            let rel = entry.strip_prefix(source).unwrap().to_path_buf();
            if rel
                .components()
                .any(|c| is_always_excluded_component(&c.as_os_str().to_string_lossy()))
            {
                continue;
            }
            rels.push(rel);
        }
    }
    Ok(rels)
}

/// Every regular file under `dir`, recursively (no symlink following into
/// directories). Used only for the non-git copy set.
fn walkdir_files(dir: &Path) -> Result<Vec<std::path::PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d)
            .with_context(|| format!("reading {}", d.display()))?
            .flatten()
        {
            let path = entry.path();
            let ft = entry.file_type()?;
            if ft.is_dir() {
                stack.push(path);
            } else if ft.is_file() {
                out.push(path);
            }
        }
    }
    Ok(out)
}

/// Recursively copy `src` to `dst` (files and directories), preserving nothing
/// but the bytes. Used for `.git`.
fn copy_tree(src: &Path, dst: &Path) -> Result<(u64, u64)> {
    let mut files = 0u64;
    let mut bytes = 0u64;
    let mut stack = vec![src.to_path_buf()];
    std::fs::create_dir_all(dst).with_context(|| format!("creating {}", dst.display()))?;
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d)
            .with_context(|| format!("reading {}", d.display()))?
            .flatten()
        {
            let path = entry.path();
            let rel = path.strip_prefix(src).unwrap();
            let target = dst.join(rel);
            let ft = entry.file_type()?;
            if ft.is_dir() {
                std::fs::create_dir_all(&target)
                    .with_context(|| format!("creating {}", target.display()))?;
                stack.push(path);
            } else if ft.is_file() {
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent).ok();
                }
                let n = std::fs::copy(&path, &target)
                    .with_context(|| format!("copying {}", path.display()))?;
                files += 1;
                bytes += n;
            }
        }
    }
    Ok((files, bytes))
}

/// Copy one Claude Code history entry into the session's `-workspace` key,
/// validating `.jsonl` line by line and dropping a trailing line that does not
/// parse (a torn final write; E-23's quiescence rule). Returns (jsonl_lines,
/// lines_dropped) for a `.jsonl` file, `(0, 0)` otherwise.
fn copy_history_jsonl(src: &Path, dst: &Path) -> Result<(u64, u64)> {
    let raw = std::fs::read(src).with_context(|| format!("reading {}", src.display()))?;
    let mut kept: Vec<u8> = Vec::with_capacity(raw.len());
    let mut lines = 0u64;
    let mut dropped = 0u64;
    for line in raw.split(|b| *b == b'\n') {
        if line.is_empty() {
            continue;
        }
        match serde_json::from_slice::<serde_json::Value>(line) {
            Ok(_) => {
                kept.extend_from_slice(line);
                kept.push(b'\n');
                lines += 1;
            }
            Err(_) => {
                // A line that does not parse is a torn write; drop it (and, as
                // it can only be the last one, everything after — there is
                // nothing after).
                dropped += 1;
            }
        }
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(dst, &kept).with_context(|| format!("writing {}", dst.display()))?;
    Ok((lines, dropped))
}

/// What an adoption would copy, computed without provisioning anything (F-15:
/// the confirmation has to say what and how big before the copy runs).
#[derive(Debug, Clone)]
pub struct AdoptPlan {
    pub source: std::path::PathBuf,
    pub files: u64,
    pub bytes: u64,
    pub git_bytes: u64,
    pub history_sessions: u64,
    pub is_git_repo: bool,
}

/// Measure what `adopt` would copy from `source`: the `.gitignore`-respecting
/// file set (with `target/` and `node_modules/` always excluded), `.git`, and
/// the Claude Code history for that directory. A read; it provisions nothing.
pub fn adopt_plan(source: &Path) -> Result<AdoptPlan> {
    let source = source
        .canonicalize()
        .with_context(|| format!("the source directory {} does not exist", source.display()))?;
    if !source.is_dir() {
        bail!("{} is not a directory", source.display());
    }
    let rels = adopt_copy_set(&source)?;
    let mut bytes = 0u64;
    for rel in &rels {
        if let Ok(m) = std::fs::symlink_metadata(source.join(rel)) {
            if m.is_file() {
                bytes += m.len();
            }
        }
    }
    let git_dir = source.join(".git");
    let mut git_bytes = 0u64;
    if git_dir.is_dir() {
        for f in walkdir_files(&git_dir)? {
            if let Ok(m) = std::fs::symlink_metadata(&f) {
                git_bytes += m.len();
            }
        }
    }
    let mut history_sessions = 0u64;
    if let Some(home) = std::env::var_os("HOME") {
        let hist = std::path::PathBuf::from(&home)
            .join(".claude/projects")
            .join(history_key(&source));
        if hist.is_dir() {
            for f in walkdir_files(&hist).unwrap_or_default() {
                if f.extension().map(|e| e == "jsonl").unwrap_or(false) {
                    history_sessions += 1;
                }
            }
        }
    }
    Ok(AdoptPlan {
        files: rels.len() as u64,
        bytes: bytes + git_bytes,
        git_bytes,
        history_sessions,
        is_git_repo: git_dir.exists(),
        source,
    })
}

/// E-23: adopt an existing host directory into a fresh session — copy its tree
/// (`.gitignore`-respecting, `target/`/`node_modules/` always excluded, `.git`
/// carried whole) and its Claude Code history (rewritten to the `-workspace`
/// key, valid JSONL to the last line) onto the session's volume, so a push and
/// a pull on another machine can `--continue` the conversation. The host
/// directory is read, never written (copy, not move).
pub async fn adopt(
    client: &ContainerdClient,
    name: &str,
    size: VolumeSize,
    agent: Agent,
    source: &Path,
) -> Result<AdoptSummary> {
    let source = source
        .canonicalize()
        .with_context(|| format!("the source directory {} does not exist", source.display()))?;
    if !source.is_dir() {
        bail!("{} is not a directory", source.display());
    }

    // The copy set and its size, BEFORE provisioning anything — a refusal must
    // not leave a half-created project.
    let rels = adopt_copy_set(&source)?;
    let git_dir = source.join(".git");
    let mut planned_bytes = 0u64;
    for rel in &rels {
        if let Ok(m) = std::fs::symlink_metadata(source.join(rel)) {
            if m.is_file() {
                planned_bytes += m.len();
            }
        }
    }
    if git_dir.is_dir() {
        for f in walkdir_files(&git_dir)? {
            if let Ok(m) = std::fs::symlink_metadata(&f) {
                planned_bytes += m.len();
            }
        }
    }
    let quota = size.bytes();
    if planned_bytes > quota {
        bail!(
            "the tree to adopt is {} but the session quota is {} ({}). \
             Exclude more (target/ and node_modules/ are already excluded), or choose a larger --size. \
             Adopting {}",
            human_size(planned_bytes),
            human_size(quota),
            size,
            source.display()
        );
    }

    // Provision the session (volume mounted, container created, persisted).
    let summary = create_with_auth(client, name, size, agent).await?;
    let mount_point = VolumePaths::from_env()?.mount_point(name);

    // Copy the tree into /workspace (the volume root).
    let mut files_copied = 0u64;
    let mut bytes_copied = 0u64;
    for rel in &rels {
        let from = source.join(rel);
        let to = mount_point.join(rel);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        // ls-files can name a path that vanished between listing and copy; skip it.
        match std::fs::symlink_metadata(&from) {
            Ok(m) if m.file_type().is_symlink() => {
                let target = std::fs::read_link(&from)?;
                let _ = std::os::unix::fs::symlink(&target, &to);
                files_copied += 1;
            }
            Ok(m) if m.is_file() => {
                let n = std::fs::copy(&from, &to)
                    .with_context(|| format!("copying {}", from.display()))?;
                files_copied += 1;
                bytes_copied += n;
            }
            _ => {}
        }
    }
    if git_dir.is_dir() {
        let (gf, gb) = copy_tree(&git_dir, &mount_point.join(".git"))?;
        files_copied += gf;
        bytes_copied += gb;
    }

    // Copy the Claude Code history for this directory, rewritten to the
    // session's `-workspace` key, valid JSONL to the last line.
    let mut history_sessions = 0u64;
    let mut history_lines_dropped = 0u64;
    let home = std::env::var_os("HOME").context("HOME is not set; cannot find the host history")?;
    let src_hist = std::path::PathBuf::from(&home)
        .join(".claude")
        .join("projects")
        .join(history_key(&source));
    if src_hist.is_dir() {
        let dst_hist = mount_point
            .join(config::VOLUME_STATE_PROJECTS)
            .join("-workspace");
        std::fs::create_dir_all(&dst_hist)
            .with_context(|| format!("creating {}", dst_hist.display()))?;
        let mut stack = vec![src_hist.clone()];
        while let Some(d) = stack.pop() {
            for entry in std::fs::read_dir(&d)
                .with_context(|| format!("reading {}", d.display()))?
                .flatten()
            {
                let path = entry.path();
                let rel = path.strip_prefix(&src_hist).unwrap();
                let target = dst_hist.join(rel);
                let ft = entry.file_type()?;
                if ft.is_dir() {
                    std::fs::create_dir_all(&target).ok();
                    stack.push(path);
                } else if ft.is_file() {
                    if path.extension().map(|e| e == "jsonl").unwrap_or(false) {
                        let (lines, dropped) = copy_history_jsonl(&path, &target)?;
                        if lines > 0 {
                            history_sessions += 1;
                        }
                        history_lines_dropped += dropped;
                    } else {
                        if let Some(parent) = target.parent() {
                            std::fs::create_dir_all(parent).ok();
                        }
                        std::fs::copy(&path, &target).ok();
                    }
                }
            }
        }
    }

    Ok(AdoptSummary {
        name: summary.name,
        container_id: summary.container_id,
        size,
        agent,
        files_copied,
        bytes_copied,
        history_sessions,
        history_lines_dropped,
        source,
    })
}

fn human_size(bytes: u64) -> String {
    const G: u64 = 1024 * 1024 * 1024;
    const M: u64 = 1024 * 1024;
    const K: u64 = 1024;
    if bytes >= G {
        format!("{:.1}GiB", bytes as f64 / G as f64)
    } else if bytes >= M {
        format!("{:.1}MiB", bytes as f64 / M as f64)
    } else if bytes >= K {
        format!("{:.1}KiB", bytes as f64 / K as f64)
    } else {
        format!("{bytes}B")
    }
}

/// Bind mounts that relocate Claude Code's session-critical state onto the
/// portable volume (M8).
///
/// Each maps a directory on the volume over the rootfs location Claude Code
/// writes to, so the conversation history and session state land on the layer
/// that travels with a bundle. Credentials (`CONTAINER_CREDENTIALS`) and the
/// identity-bearing `/root/.claude.json` are deliberately absent — they stay on
/// the rootfs so they cannot travel (D-02). See `docs/state-locality.md`.
fn session_state_mounts(mount_point: &std::path::Path) -> Vec<BindMount> {
    vec![
        BindMount::read_write(
            mount_point.join(config::VOLUME_STATE_PROJECTS),
            config::CONTAINER_CLAUDE_PROJECTS,
        ),
        BindMount::read_write(
            mount_point.join(config::VOLUME_STATE_SESSIONS),
            config::CONTAINER_CLAUDE_SESSIONS,
        ),
    ]
}

/// What `create` produced, for the CLI to report.
#[derive(Debug, Clone)]
pub struct ProjectSummary {
    pub name: String,
    pub container_id: String,
    pub volume_path: String,
    pub size: VolumeSize,
    pub agent: Agent,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The labels `list` and `reconcile_orphans` key on must actually be written.
    ///
    /// The previous version asserted a prefix on three string constants declared
    /// in the same file — true by construction, and the entire label-writing
    /// block in `create` could be deleted with it green, while `list` would show
    /// no projects and `reconcile_orphans` would treat every volume as an orphan
    /// and release its mount (F-58). Proven to fail when the labels are dropped.
    #[test]
    fn create_writes_the_labels_list_and_reconcile_depend_on() {
        let labels = project_labels(
            "demo",
            "/mnt/demo",
            VolumeSize::Medium,
            Agent::ClaudeCode,
            Some(netns::Allocation { index: 3 }),
        );
        // NET-02: the session's network is recorded on the container, like
        // every other piece of project state.
        assert_eq!(labels.get(LABEL_NETNS).map(String::as_str), Some("3"));

        assert_eq!(labels.get(LABEL_PROJECT).map(String::as_str), Some("demo"));
        assert_eq!(
            labels.get(LABEL_VOLUME).map(String::as_str),
            Some("/mnt/demo")
        );
        assert_eq!(labels.get(LABEL_SIZE).map(String::as_str), Some("2GB"));

        for key in labels.keys() {
            assert!(key.starts_with("nemr."), "{key} should be namespaced");
        }
    }

    #[test]
    fn container_id_is_prefixed() {
        assert_eq!(config::container_id("demo"), "nemr-demo");
    }
}

/// Resolve a project name to its container, failing clearly if absent.
async fn resolve(client: &ContainerdClient, name: &str) -> Result<String> {
    crate::engine::volume::validate_name(name)
        .with_context(|| format!("invalid project name {name:?}"))?;

    let container_id = config::container_id(name);
    if !client.container_exists(&container_id).await? {
        bail!(
            "no project named {name:?}.\n\
             Create it first: nemr create {name} --size 2GB"
        );
    }
    Ok(container_id)
}

/// Start a project's container (Milestone 5).
///
/// PID 1 is the supervisor from PROC-01; no interactive session is created
/// here. `attach` is what gives a shell.
pub async fn start(client: &ContainerdClient, name: &str) -> Result<u32> {
    // E-21: the bind source must exist before the task starts. On a machine
    // that has never logged in that is the placeholder; on one that has, the
    // real file, untouched.
    auth::ensure_host_credential_file()?;
    let container_id = resolve(client, name).await?;

    // VOL-06. Container records live in containerd's database and survive a
    // reboot; mounts and loop devices do not. Starting without checking gives
    // the container an empty /workspace backed by whatever filesystem the mount
    // point directory happens to sit on — the host root filesystem, with no
    // quota. Nothing errors, so a user can work an entire session believing
    // they are writing to their project. That is a silent VOL-05 violation, and
    // this check is what closes it.
    ensure_volume_mounted(name)?;

    // F-128. The helper mounted the volume in the host namespace; runc runs in
    // rootlesskit's rslave namespace and sees it only if it propagated in, which
    // requires the host-side mount to be shared. On WSL2 `/` is private, so it
    // does not — and the M8 session-state bind sources under it
    // (`.nemr-state/{projects,sessions}`) then do not exist for runc, which
    // fails start with an opaque "no such file or directory". Refuse here,
    // before that riddle, naming the fix.
    let mount_point = crate::engine::volume::VolumePaths::from_env()?.mount_point(name);
    if crate::engine::volume::mount_propagation(&mount_point)
        == crate::engine::volume::MountPropagation::Private
    {
        bail!(
            "the project volume at {} is a private mount, so it does not \
             propagate into the container runtime's namespace: the session-state \
             mounts under it would be invisible and the task would fail to start \
             with an opaque 'no such file or directory'.\n\
             The host's root mount must be shared. A standard systemd host makes \
             it so at boot; WSL2 does not (E-10). Fix it for this and every \
             future boot by re-running ./scripts/setup_host.sh, which installs \
             the propagation unit — or immediately, for the current session:\n\n    \
             sudo mount --make-rshared /\n    \
             systemctl --user restart containerd-rootless.service nemrd.service",
            mount_point.display()
        );
    }

    // AC-5.3: starting an already-running project is a clear error, not a
    // second task or a silent no-op.
    match client.task_state(&container_id).await? {
        state if state.is_running() => bail!(
            "project {name:?} is already running.\n\
             Attach to it with: nemr attach {name}"
        ),
        crate::containerd::containers::TaskState::Stopped => {
            // A task that exited but was never reaped would block a new one.
            client.stop_task(&container_id).await?;
        }
        _ => {}
    }

    // NET-02 migration, BEFORE the task exists: the OCI spec is frozen into the
    // container record at create time and read again at every task start, so a
    // project created before NET-02 asks for no network namespace and its task
    // joins rootlesskit's — for ever, whatever the engine does afterwards. The
    // first migration recorded an allocation and then wired it, which built a
    // session network inside the SHARED namespace and failed with "RTNETLINK
    // answers: File exists" against rootlesskit's own default route. A project
    // that used to start stopped starting.
    if client
        .ensure_own_network_namespace(&container_id)
        .await
        .with_context(|| format!("migrating {name:?} to a per-session network namespace"))?
    {
        // Tagged so it reaches the user (F-98): the project's container record
        // was changed, once, and silently changing someone's project is not the
        // same as changing it.
        tracing::warn!(
            nemr_audit = "warning",
            "[nemr] {name}: gave this project its own network namespace (NET-02 migration)"
        );
    }

    // D-02 (f) / F-14 migration, also BEFORE the task exists and for the same
    // reason: the credential mount is frozen in the record of every project
    // created before F-14 as a single-FILE bind of `.credentials.json`, which
    // a login's rename escapes (the human-arm failure, E-21). Rewrite it to a
    // read-write bind of the host's dedicated credential DIRECTORY over
    // `/root/.claude`, so every write — in place or by rename — lands on the
    // host. Idempotent, and it reports the change once. The directory and its
    // `projects`/`sessions` mount points exist by now (ensure above).
    // NEMR_TEST_SKIP_F14_MIGRATION leaves a pre-F14 single-file bind in place
    // at start, so a genuinely file-bound RUNNING session can be produced — the
    // shape the migration otherwise always repairs — and the file bind's
    // failure (a login's rename refused on the mount point) is provable end to
    // end. A test seam like NEMR_TEST_PRE_F14; production never sets it.
    if std::env::var_os("NEMR_TEST_SKIP_F14_MIGRATION").is_none()
        && client
            .ensure_credential_dir_bind(
                &container_id,
                &auth::host_credential_dir()?.to_string_lossy(),
            )
            .await
            .with_context(|| format!("migrating {name:?} to a directory credential mount"))?
    {
        tracing::warn!(
            nemr_audit = "warning",
            "[nemr] {name}: the credential is now bound as a directory over /root/.claude, so \
             Claude Code's login writes (which rename) land on this host (F-14 migration)"
        );
    }

    let pid = client.start_task(&container_id).await?;

    // NET-02: the task now has its own empty network namespace. Wire it before
    // anything else touches the network — a session with an address and no
    // route is a session where Claude Code cannot reach the API, and the
    // failure would surface much later as an authentication error.
    //
    // Not conditional on a lucky read. The first version was
    // `find_container(...).ok().and_then(...)`, which made two very different
    // situations silent: a containerd hiccup, and a project with no allocation
    // recorded. Both then started a session into a namespace containing nothing
    // at all — no address, no route, not even loopback up — and reported
    // success. Failing to wire was fatal while not knowing whether to wire was
    // silent, which is exactly backwards.
    if let Err(e) = wire_session(client, name, pid).await {
        // Tear the task down rather than leave a session that looks started and
        // has no network: a half-connected session is worse than a refusal,
        // because the user only finds out when something fails.
        //
        // And say which of those actually happened. Claiming "the task was
        // stopped" without checking is the same class of untruth one level down.
        let note = match client.stop_task(&container_id).await {
            Ok(_) => "so the task was stopped".to_string(),
            Err(stop_err) => format!(
                "and the task could NOT be stopped afterwards ({stop_err:#}) — it is running \
                 with no network. Stop it with: nemr stop {name}"
            ),
        };
        return Err(e.context(format!(
            "starting {name:?}: its network could not be configured, {note}"
        )));
    }

    // Declared packages the session does not have (F-118): a rare signal now
    // that packages survive stop/start, so it carries information when it
    // fires — after an import, or after a failed provision. One line, tagged
    // to reach the user (F-98); the names live behind provision itself. Never
    // fails the start, and "could not tell" is reported as itself rather than
    // read as "none missing".
    match missing_declared_packages(client, name).await {
        Ok(missing) if !missing.is_empty() => {
            tracing::warn!(
                nemr_audit = "warning",
                "[nemr] {name}: {} declared package(s) not installed in this session — run: \
                 nemr provision {name}",
                missing.len()
            );
        }
        Ok(_) => {}
        Err(error) => {
            tracing::warn!(
                nemr_audit = "warning",
                "[nemr] {name}: could not check declared packages ({error:#})"
            );
        }
    }

    // F-131: the first `claude` in this session must open ready — no theme
    // picker, no login method, no trust dialog — because the user is already
    // signed in on this host. Seed the session's Claude Code config from the
    // host's, by allowlist, once (the write is guarded by `test -e`, so a
    // session that has run `claude` already is left alone). Best effort and
    // said when it fails: a session that opens with onboarding is degraded,
    // not broken, and the start must not fail for it.
    match seed_session_config(client, name).await {
        Ok(true) => tracing::info!("[nemr] {name}: seeded Claude Code's config (onboarding complete, workspace trusted, identity from the host)"),
        Ok(false) => {}
        Err(error) => tracing::warn!(
            nemr_audit = "warning",
            "[nemr] {name}: could not seed Claude Code's config ({error:#}); the first `claude` may show onboarding"
        ),
    }

    // Re-apply declared forwards (WP-M). They are derived state: torn down on
    // stop, and gone entirely after a rootlesskit restart, so the declaration
    // is applied rather than assumed live. A port that cannot be bound is
    // reported by the caller and skipped — one unavailable port must not stop a
    // session from starting, and silently dropping it would be worse.
    for problem in apply_declared_ports(client, name).await {
        // Tagged so the audit stream carries it to the USER (F-98). An untagged
        // warn goes only to the daemon log, so a port that silently failed to
        // forward looked like a working session until someone opened a browser.
        tracing::warn!(
            nemr_audit = "warning",
            "[nemr] port NOT forwarded: {problem}"
        );
    }

    Ok(pid)
}

/// Stop a project's container (Milestone 5).
///
/// Returns how the task actually stopped, so the caller can distinguish a
/// clean shutdown from one that had to be killed (PROC-06). Reporting only
/// success is what let a supervisor that ignored SIGTERM go unnoticed for the
/// whole of Phase 1.
pub async fn stop(client: &ContainerdClient, name: &str) -> Result<StopOutcome> {
    let container_id = resolve(client, name).await?;

    // AC-5.3: stopping an already-stopped project must fail clearly rather
    // than report success for work it did not do.
    if client.task_state(&container_id).await? == crate::containerd::containers::TaskState::None {
        bail!(
            "project {name:?} is not running.\n\
             Start it with: nemr start {name}"
        );
    }

    // Drop live forwards but keep the declaration (WP-M): a stopped project
    // must not hold a host port against another project, while the port still
    // belongs to it across runs.
    withdraw_declared_ports(client, name).await;

    let outcome = client.stop_task(&container_id).await;

    // NET-02: the session's namespace dies with the task, taking the container
    // end of the veth with it; this removes our end and the NAT rule. After the
    // task, so nothing is torn down while it might still be serving — and only
    // if the task really stopped. Tearing the network out from under a session
    // that is still running would take away its address and its egress while it
    // was working, which is worse than the failed stop we are already reporting.
    if outcome.is_ok() {
        if let Some(alloc) = find_container(client, name)
            .await
            .ok()
            .and_then(|c| allocation_from_labels(&c.labels))
        {
            let _ = tokio::task::spawn_blocking(move || netns::disconnect_session(alloc)).await;
        }
    }

    outcome
}

/// Remove FIFO directories left behind by attach processes that are gone.
///
/// A clean exit removes its own directory. One killed outright cannot, so these
/// accumulate in `$XDG_RUNTIME_DIR` (NFR-03). Each directory carries the PID
/// that created it, so a live session's directory is never touched — deleting
/// one belonging to a long-running attach would break it, which rules out
/// simpler age-based sweeping.
///
/// Best-effort throughout: this is tidying, and must never obstruct an attach.
fn sweep_stale_attach_dirs() {
    let Some(runtime_dir) = std::env::var_os("XDG_RUNTIME_DIR") else {
        return;
    };
    let base = std::path::PathBuf::from(runtime_dir).join("nemr");
    let Ok(entries) = std::fs::read_dir(&base) else {
        return;
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };

        // attach-<pid>-<nanos>
        let Some(pid) = name
            .strip_prefix("attach-")
            .and_then(|rest| rest.split('-').next())
            .and_then(|pid| pid.parse::<u32>().ok())
        else {
            continue;
        };

        // /proc/<pid> existing is the liveness test; absent means the creator
        // is gone and the directory is safe to remove.
        if !std::path::Path::new(&format!("/proc/{pid}")).exists() {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Ensure a project's volume is mounted, remounting it if not (VOL-06).
///
/// Remount rather than refuse: the backing file is intact and the privileged
/// helper already knows how to attach and mount it, so requiring the user to
/// repair this by hand would defeat the portability the product exists for.
/// Failure to remount *is* fatal — proceeding is what this guards against.
#[tracing::instrument(name = "ensure_volume_mounted", skip_all, fields(project = %name))]
pub fn ensure_volume_mounted(name: &str) -> Result<()> {
    let paths = VolumePaths::from_env()?;
    let mount_point = paths.mount_point(name);

    // The VOL-05 decision point. Log what was checked and what the answer was —
    // not just the action taken. VOL-05 was invisible in the log precisely
    // because "proceeding" and "proceeding against the wrong filesystem" printed
    // the same nothing.
    let mounted = crate::engine::volume::is_mounted(&mount_point);
    tracing::debug!(
        mount_point = %mount_point.display(),
        mounted,
        backing_device = %crate::engine::volume::backing_device(&mount_point)
            .unwrap_or_else(|| "<none>".into()),
        "checked whether the project volume is mounted"
    );

    if mounted {
        // F-28: "something is mounted here" is not "our volume is mounted here".
        // The mount point is a plain directory until a mount lands on it, and a
        // wrong mount is indistinguishable from the right one by presence
        // alone — the container is simply handed a foreign filesystem, usually
        // an empty one, and reports nothing. That is the shape F-63a was seen
        // in: mounted=true, a real loop device, and a volume containing only
        // lost+found.
        //
        // Refuse rather than proceed. A project that will not start is
        // recoverable; a session silently restored onto someone else's volume
        // is not.
        let expected = paths.image_file(name);
        let actual = crate::engine::volume::mounted_image_path(&mount_point);
        match crate::engine::volume::mount_identity(&expected, actual.as_deref()) {
            Ok(()) => {}
            Err(crate::engine::volume::MountIdentityError::WrongVolume { actual }) => bail!(
                "the filesystem mounted at {} is not this project's volume.\n\
                 expected backing image: {}\n\
                 actually backed by:     {}\n\
                 Refusing to start: continuing would hand the container a volume \
                 belonging to something else. Run `nemr reconcile`, then try again.",
                mount_point.display(),
                expected.display(),
                actual.display()
            ),
            Err(crate::engine::volume::MountIdentityError::NotLoopBacked) => {
                // Not a loop-backed mount at all — so whatever is there, it is
                // not a volume this engine provisioned.
                bail!(
                    "the filesystem mounted at {} is not loop-backed, so it is not a \
                     nemr volume.\n\
                     Refusing to start: the container would run against an \
                     unmanaged filesystem with no quota (VOL-05).\n\
                     Inspect it with: findmnt {}",
                    mount_point.display(),
                    mount_point.display()
                )
            }
        }
        return Ok(());
    }

    let image = paths.image_file(name);
    if !image.exists() {
        bail!(
            "project {name:?} has no backing file at {}.\n\
             The volume is gone; the project cannot be started. Delete it with \
             `nemr delete {name}` and create it again.",
            image.display()
        );
    }

    // The size preset is needed only for logging inside the helper; the volume
    // already exists and is not resized here.
    let size = read_recorded_size(&paths, name).unwrap_or(VolumeSize::DEFAULT);

    audit_remount(name, &mount_point);
    std::fs::create_dir_all(&mount_point)
        .with_context(|| format!("failed to recreate mount point {}", mount_point.display()))?;

    HelperOps::new()
        .attach_and_mount(name, size)
        .with_context(|| {
            format!(
                "failed to remount the volume for project {name:?}.\n\
                 Refusing to start: the container would otherwise run against \
                 {} on the host filesystem, with no quota and none of the \
                 project's data.",
                mount_point.display()
            )
        })?;

    // Confirm the remount actually landed, and say which device backs it. This
    // is the line that turns VOL-05 from "found after a reboot by hand" into
    // "obvious on the first run": a working directory backed by the host root
    // device instead of a loop device is visible right here.
    let device = crate::engine::volume::backing_device(&mount_point);
    tracing::debug!(
        mount_point = %mount_point.display(),
        remounted = crate::engine::volume::is_mounted(&mount_point),
        backing_device = %device.clone().unwrap_or_else(|| "<none>".into()),
        "remounted the project volume"
    );
    tracing::info!(
        "remounted volume for {name:?} from {} ({})",
        image.display(),
        device.unwrap_or_else(|| "unknown device".into())
    );

    Ok(())
}

/// Best-effort recovery of the size a volume was created with.
///
/// Derived from the backing file's apparent size, which is exactly the preset
/// requested at creation — the file is sparse, so this costs nothing to read
/// and does not depend on containerd being reachable.
/// The agent recorded on a project's container labels, defaulting to Claude
/// Code when the label is absent (a project created before the field existed)
/// or holds an unknown value (a downgrade reading a newer project — better the
/// default than a hard failure on a read path).
pub fn agent_from_labels(labels: &std::collections::HashMap<String, String>) -> Agent {
    labels
        .get(LABEL_AGENT)
        .and_then(|id| id.parse::<Agent>().ok())
        .unwrap_or_else(Agent::default_agent)
}

fn read_recorded_size(paths: &VolumePaths, name: &str) -> Option<VolumeSize> {
    let length = std::fs::metadata(paths.image_file(name)).ok()?.len();
    VolumeSize::all().into_iter().find(|s| s.bytes() == length)
}

fn audit_remount(name: &str, mount_point: &std::path::Path) {
    tracing::info!(
        "[nemr:volume] volume for {name:?} is not mounted at {}; remounting (VOL-06)",
        mount_point.display()
    );
}

/// Whether a project is currently running.
pub async fn is_running(client: &ContainerdClient, name: &str) -> Result<bool> {
    let container_id = config::container_id(name);
    Ok(client.task_state(&container_id).await?.is_running())
}

/// Run one command in a running project and capture its stdout and exit code.
///
/// A non-interactive counterpart to [`attach`]: no TTY, no stdin, output
/// collected rather than streamed. Used by health checks and by the regression
/// suite to read the container's own view of a path (e.g. to prove that
/// relocated session state is visible where Claude Code writes it), without
/// depending on the streaming attach machinery.
pub async fn exec_capture(
    client: &ContainerdClient,
    name: &str,
    argv: &[&str],
) -> Result<(u32, String)> {
    use crate::containerd::containers::ExecIo;
    use crate::engine::tty;
    use std::io::Write;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    let container_id = resolve(client, name).await?;
    if !client.task_state(&container_id).await?.is_running() {
        bail!(
            "project {name:?} is not running, so there is nothing to run a command in.\n    \
             nemr start {name}\n\
             Check what state it is in with: nemr status {name}"
        );
    }

    let exec_id = format!(
        "capture-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let io_dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .context("XDG_RUNTIME_DIR is not set; cannot place exec FIFOs")?
        .join("nemr")
        .join(&exec_id);
    std::fs::create_dir_all(&io_dir)
        .with_context(|| format!("failed to create {}", io_dir.display()))?;

    let io = ExecIo {
        stdin: io_dir.join("stdin"),
        stdout: io_dir.join("stdout"),
        stderr: Some(io_dir.join("stderr")),
        terminal: false,
    };
    tty::make_fifo(&io.stdin)?;
    tty::make_fifo(&io.stdout)?;
    if let Some(stderr) = &io.stderr {
        tty::make_fifo(stderr)?;
    }

    let process = serde_json::json!({
        "terminal": false,
        "user": { "uid": 0, "gid": 0 },
        "args": argv,
        "env": [
            "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
            "HOME=/root",
        ],
        "cwd": "/",
        "noNewPrivileges": true
    });

    client
        .exec_process(&container_id, &exec_id, process, &io)
        .await?;

    let stdin_fifo = tty::open_fifo(&io.stdin)?;
    let stdout_fifo = tty::open_fifo(&io.stdout)?;
    let stderr_fifo = io.stderr.as_ref().map(|p| tty::open_fifo(p)).transpose()?;

    client.start_exec(&container_id, &exec_id).await?;

    // No stdin: close our write end so the process sees EOF immediately.
    drop(stdin_fifo);
    let _ = client.close_exec_stdin(&container_id, &exec_id).await;

    // Collect stdout on a thread until the exec exits (the FIFO is O_RDWR, so it
    // never reports EOF on its own — same reason attach uses a stop flag).
    let collected = Arc::new(Mutex::new(Vec::<u8>::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let out_writer = SharedWriter(collected.clone());
    let out_stop = stop.clone();
    let out_thread = std::thread::spawn(move || {
        tty::pump_until_stopped(stdout_fifo, out_writer, out_stop, None)
    });
    let err_thread = stderr_fifo.map(|fifo| {
        let stop = stop.clone();
        std::thread::spawn(move || tty::pump_until_stopped(fifo, std::io::sink(), stop, None))
    });

    let exit = client.wait_exec(&container_id, &exec_id).await?;
    let _ = client.delete_exec(&container_id, &exec_id).await;

    stop.store(true, Ordering::Relaxed);
    let _ = out_thread.join();
    if let Some(handle) = err_thread {
        let _ = handle.join();
    }
    let _ = std::fs::remove_dir_all(&io_dir);

    let stdout = collected
        .lock()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default();

    // A tiny local Write adapter, so pump_until_stopped can collect into a Vec.
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);
    impl Write for SharedWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if let Ok(mut guard) = self.0.lock() {
                guard.extend_from_slice(buf);
            }
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    Ok((exit, stdout))
}

/// Attach an interactive session to a running project (Milestone 5).
///
/// Per PROC-02 this is a **task exec with its own TTY**, not a connection to
/// PID 1's terminal. Exiting the shell ends only this exec; the project keeps
/// running, and concurrent attaches get independent terminals.
/// A running attach exec, with the FIFO handles to pump and the ids to control
/// it. Produced by [`attach_exec_start`] for the daemon (E-09), which streams
/// between these FIFOs and a gRPC client rather than the local terminal.
pub struct AttachExec {
    pub container_id: String,
    pub exec_id: String,
    pub io_dir: std::path::PathBuf,
    pub stdin_fifo: std::fs::File,
    pub stdout_fifo: std::fs::File,
    pub stderr_fifo: Option<std::fs::File>,
    /// Whether the exec was created with a pty (interactive) or pipes.
    pub terminal: bool,
}

/// Set up and start an attach exec: resolve the container, create the FIFOs,
/// launch the agent's login shell, and apply the initial window size. Returns
/// the handles for the caller to pump and control.
///
/// Factored out of the terminal-bound `attach` so the daemon can drive the same
/// exec while streaming its IO over gRPC. `interactive`/`rows`/`cols` come from
/// the client's own terminal (the daemon has none), replacing the local
/// `stdin_is_terminal()` / `window_size()` calls.
pub async fn attach_exec_start(
    client: &ContainerdClient,
    name: &str,
    interactive: bool,
    rows: u16,
    cols: u16,
) -> Result<AttachExec> {
    use crate::containerd::containers::ExecIo;
    use crate::engine::tty;

    let container_id = resolve(client, name).await?;
    if !client.task_state(&container_id).await?.is_running() {
        bail!(
            "project {name:?} is not running.\n\
             Start it first: nemr start {name}"
        );
    }

    let exec_id = format!(
        "attach-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    sweep_stale_attach_dirs();

    let io_dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .context("XDG_RUNTIME_DIR is not set; cannot place attach FIFOs")?
        .join("nemr")
        .join(&exec_id);
    std::fs::create_dir_all(&io_dir)
        .with_context(|| format!("failed to create {}", io_dir.display()))?;

    let io = ExecIo {
        stdin: io_dir.join("stdin"),
        stdout: io_dir.join("stdout"),
        stderr: (!interactive).then(|| io_dir.join("stderr")),
        terminal: interactive,
    };
    tty::make_fifo(&io.stdin)?;
    tty::make_fifo(&io.stdout)?;
    if let Some(stderr) = &io.stderr {
        tty::make_fifo(stderr)?;
    }

    let process = serde_json::json!({
        "terminal": interactive,
        "user": { "uid": 0, "gid": 0 },
        "args": ["/bin/bash", "-l"],
        "env": [
            "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
            "HOME=/root",
            "TERM=".to_string() + &std::env::var("TERM").unwrap_or_else(|_| "xterm".into()),
            "USE_BUILTIN_RIPGREP=0".to_string(),
            PROMPT_TIDY.to_string(),
        ],
        "cwd": config::CONTAINER_WORKDIR,
        "capabilities": {
            "bounding":  ["CAP_CHOWN","CAP_DAC_OVERRIDE","CAP_FSETID","CAP_FOWNER","CAP_MKNOD",
                          "CAP_NET_RAW","CAP_SETGID","CAP_SETUID","CAP_SETFCAP","CAP_SETPCAP",
                          "CAP_NET_BIND_SERVICE","CAP_SYS_CHROOT","CAP_KILL","CAP_AUDIT_WRITE"],
            "effective": ["CAP_CHOWN","CAP_DAC_OVERRIDE","CAP_FSETID","CAP_FOWNER","CAP_MKNOD",
                          "CAP_NET_RAW","CAP_SETGID","CAP_SETUID","CAP_SETFCAP","CAP_SETPCAP",
                          "CAP_NET_BIND_SERVICE","CAP_SYS_CHROOT","CAP_KILL","CAP_AUDIT_WRITE"],
            "permitted": ["CAP_CHOWN","CAP_DAC_OVERRIDE","CAP_FSETID","CAP_FOWNER","CAP_MKNOD",
                          "CAP_NET_RAW","CAP_SETGID","CAP_SETUID","CAP_SETFCAP","CAP_SETPCAP",
                          "CAP_NET_BIND_SERVICE","CAP_SYS_CHROOT","CAP_KILL","CAP_AUDIT_WRITE"]
        },
        "noNewPrivileges": true
    });

    client
        .exec_process(&container_id, &exec_id, process, &io)
        .await?;

    let stdin_fifo = tty::open_fifo(&io.stdin)?;
    let stdout_fifo = tty::open_fifo(&io.stdout)?;
    let stderr_fifo = io.stderr.as_ref().map(|p| tty::open_fifo(p)).transpose()?;

    client.start_exec(&container_id, &exec_id).await?;

    if interactive && rows > 0 && cols > 0 {
        let _ = client
            .resize_pty(&container_id, &exec_id, cols as u32, rows as u32)
            .await;
    }

    Ok(AttachExec {
        container_id,
        exec_id,
        io_dir,
        stdin_fifo,
        stdout_fifo,
        stderr_fifo,
        terminal: interactive,
    })
}

pub async fn attach(client: &ContainerdClient, name: &str) -> Result<u32> {
    use crate::containerd::containers::ExecIo;
    use crate::engine::tty;

    let container_id = resolve(client, name).await?;

    // AC-5.3: attaching to a project that was never started must fail clearly,
    // not hang waiting for a task that does not exist.
    if !client.task_state(&container_id).await?.is_running() {
        bail!(
            "project {name:?} is not running.\n\
             Start it first: nemr start {name}"
        );
    }

    // A unique exec id per attach, so concurrent sessions do not collide. The
    // PID is included so a directory left behind by a killed process can be
    // identified and swept later — see `sweep_stale_attach_dirs`.
    let exec_id = format!(
        "attach-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );

    sweep_stale_attach_dirs();

    let io_dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .context("XDG_RUNTIME_DIR is not set; cannot place attach FIFOs")?
        .join("nemr")
        .join(&exec_id);
    std::fs::create_dir_all(&io_dir)
        .with_context(|| format!("failed to create {}", io_dir.display()))?;

    // A pty only when stdin really is a terminal. This is what `docker exec -t`
    // does, and it matters for more than cosmetics: a pty has no EOF, so a
    // scripted `echo cmd | nemr attach` could never tell the shell its input had
    // finished. Neither writing EOT nor containerd's CloseIO ends a pty-backed
    // session — both were tried, and both hung indefinitely. Without a terminal
    // stdin is an ordinary pipe, closing it is a real EOF, and the shell exits
    // on its own with its own status.
    let use_terminal = tty::stdin_is_terminal();

    let io = ExecIo {
        stdin: io_dir.join("stdin"),
        stdout: io_dir.join("stdout"),
        // A pty merges stderr into the same stream; only a pipe-backed session
        // needs a separate one.
        stderr: (!use_terminal).then(|| io_dir.join("stderr")),
        terminal: use_terminal,
    };
    tty::make_fifo(&io.stdin)?;
    tty::make_fifo(&io.stdout)?;
    if let Some(stderr) = &io.stderr {
        tty::make_fifo(stderr)?;
    }

    let process = serde_json::json!({
        "terminal": use_terminal,
        "user": { "uid": 0, "gid": 0 },
        "args": ["/bin/bash", "-l"],
        "env": [
            "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
            "HOME=/root",
            "TERM=".to_string() + &std::env::var("TERM").unwrap_or_else(|_| "xterm".into()),
            "USE_BUILTIN_RIPGREP=0".to_string(),
            PROMPT_TIDY.to_string(),
        ],
        "cwd": config::CONTAINER_WORKDIR,
        "capabilities": {
            "bounding":  ["CAP_CHOWN","CAP_DAC_OVERRIDE","CAP_FSETID","CAP_FOWNER","CAP_MKNOD",
                          "CAP_NET_RAW","CAP_SETGID","CAP_SETUID","CAP_SETFCAP","CAP_SETPCAP",
                          "CAP_NET_BIND_SERVICE","CAP_SYS_CHROOT","CAP_KILL","CAP_AUDIT_WRITE"],
            "effective": ["CAP_CHOWN","CAP_DAC_OVERRIDE","CAP_FSETID","CAP_FOWNER","CAP_MKNOD",
                          "CAP_NET_RAW","CAP_SETGID","CAP_SETUID","CAP_SETFCAP","CAP_SETPCAP",
                          "CAP_NET_BIND_SERVICE","CAP_SYS_CHROOT","CAP_KILL","CAP_AUDIT_WRITE"],
            "permitted": ["CAP_CHOWN","CAP_DAC_OVERRIDE","CAP_FSETID","CAP_FOWNER","CAP_MKNOD",
                          "CAP_NET_RAW","CAP_SETGID","CAP_SETUID","CAP_SETFCAP","CAP_SETPCAP",
                          "CAP_NET_BIND_SERVICE","CAP_SYS_CHROOT","CAP_KILL","CAP_AUDIT_WRITE"]
        },
        "noNewPrivileges": true
    });

    client
        .exec_process(&container_id, &exec_id, process, &io)
        .await?;

    // Open both FIFOs before starting, so no output is lost in the gap between
    // the process starting and us being ready to read.
    let stdin_fifo = tty::open_fifo(&io.stdin)?;
    let stdout_fifo = tty::open_fifo(&io.stdout)?;
    let stderr_fifo = io.stderr.as_ref().map(|p| tty::open_fifo(p)).transpose()?;

    // Raw mode is enabled only once the exec is about to run, and the guard
    // restores the terminal on every exit path below.
    let _raw = tty::RawMode::enable()?;

    client.start_exec(&container_id, &exec_id).await?;

    if let Some((width, height)) = tty::window_size() {
        let _ = client
            .resize_pty(&container_id, &exec_id, width, height)
            .await;
    }

    // Blocking IO off the async runtime. The gRPC side stays async; mixing is
    // simpler here than making FIFO reads async.
    //
    // The stdin pump is a tracked task rather than a detached thread because
    // its *completion* is load-bearing: when local input is exhausted the exec
    // must be told, or a shell reading piped input never sees EOF and never
    // exits. See the select loop below.
    // A second handle on the stdin FIFO, kept so EOT can be written after the
    // pump has consumed local input and given up ownership of its copy.
    // Kept so end-of-input can be signalled after the pump has finished with
    // its own copy. Held in an Option because, for a pipe-backed session,
    // *dropping* it is the signal: the shim only sees EOF on the FIFO once
    // every write end is closed, and ours would otherwise hold it open forever.
    let mut eof_handle = Some(
        stdin_fifo
            .try_clone()
            .context("failed to duplicate the stdin FIFO handle")?,
    );

    // A plain thread, deliberately not `spawn_blocking`.
    //
    // Interactively this pump blocks in `read()` on the user's terminal, which
    // never reaches EOF, so it can never finish. Tokio cannot cancel a blocking
    // task once it has started, and dropping the runtime *waits* for the
    // blocking pool to drain — so the process could not exit after the shell
    // did. The symptom was `exit` printing "logout" and then hanging forever,
    // leaving the terminal unusable.
    //
    // A detached OS thread dies with the process instead. Completion is
    // reported over a channel, since `select!` still needs to know when local
    // input has run out.
    let (stdin_done_tx, stdin_done) = tokio::sync::oneshot::channel::<()>();
    std::thread::spawn(move || {
        tty::pump(std::io::stdin(), stdin_fifo);
        if std::env::var_os("NEMR_DEBUG_ATTACH").is_some() {
            eprintln!("[debug] stdin pump finished");
        }
        // Failure means the receiver is gone because the session already
        // ended, which is not an error.
        let _ = stdin_done_tx.send(());
    });
    tokio::pin!(stdin_done);
    // The output pump needs an explicit stop signal rather than relying on EOF;
    // see `pump_until_stopped` for why a FIFO opened O_RDWR never reports one.
    let output_stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let pump_stop = output_stop.clone();

    // Only an interactive session can leave the local terminal in a bad state,
    // so only that one is worth watching.
    let modes = use_terminal
        .then(|| std::sync::Arc::new(std::sync::Mutex::new(tty::ModeTracker::default())));
    let pump_modes = modes.clone();

    let from_container = std::thread::spawn(move || {
        tty::pump_until_stopped(stdout_fifo, std::io::stdout(), pump_stop, pump_modes);
    });

    let errors_from_container = stderr_fifo.map(|fifo| {
        let stop = output_stop.clone();
        std::thread::spawn(move || {
            tty::pump_until_stopped(fifo, std::io::stderr(), stop, None);
        })
    });

    // Forward window resizes for as long as the session lasts.
    let resize_client = container_id.clone();
    let resize_exec = exec_id.clone();
    let mut winch = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())
        .context("failed to install SIGWINCH handler")?;

    // Fallback timer, armed only if signalling EOF the polite way does not get
    // the process to exit. Created up front so `select!` always has something
    // to poll; it does nothing until armed.
    let fallback = tokio::time::sleep(std::time::Duration::from_secs(0));
    tokio::pin!(fallback);
    let mut fallback_armed = false;
    let mut eof_signalled = false;

    let exit_code = loop {
        tokio::select! {
            status = client.wait_exec(&container_id, &exec_id) => break status?,

            _ = winch.recv() => {
                if let Some((width, height)) = tty::window_size() {
                    let _ = client.resize_pty(&resize_client, &resize_exec, width, height).await;
                }
            }

            // Local input ran out — a pipe or heredoc rather than a terminal.
            // Signal it now, while still waiting: doing it *after* `wait_exec`
            // returns deadlocks, since the shell will not exit until it sees
            // EOF and we would not send EOF until it exits. Interactively this
            // never shows, because a terminal's stdin never reaches EOF.
            _ = &mut stdin_done, if !eof_signalled => {
                eof_signalled = true;
                if std::env::var_os("NEMR_DEBUG_ATTACH").is_some() {
                    eprintln!("[debug] local stdin exhausted; sending EOT");
                }

                if use_terminal {
                    // On a pty, EOF is EOT (0x04) written into the terminal
                    // rather than a closed descriptor. Best-effort: whether it
                    // ends the session depends on the program, hence the
                    // fallback below.
                    if let Some(fifo) = eof_handle.as_ref() {
                        use std::io::Write;
                        let mut fifo = fifo;
                        let _ = fifo.write_all(&[0x04]);
                        let _ = fifo.flush();
                    }

                    fallback.as_mut().reset(
                        tokio::time::Instant::now() + std::time::Duration::from_secs(10),
                    );
                    fallback_armed = true;
                } else {
                    // A pipe. Dropping our handle is what produces the EOF: the
                    // shim's copier is reading this FIFO, and a FIFO only
                    // reports EOF once *every* write end is closed — including
                    // the one this process holds. CloseIO alone did not end the
                    // session, because our handle kept the pipe alive.
                    drop(eof_handle.take());
                    let _ = client.close_exec_stdin(&container_id, &exec_id).await;
                }
            }

            // The process ignored EOT — not a shell, or one not reading stdin.
            // Force the issue rather than waiting forever; the exit status is
            // then SIGHUP-flavoured, but a wrong code beats a hang.
            _ = &mut fallback, if fallback_armed => {
                fallback_armed = false;
                if std::env::var_os("NEMR_DEBUG_ATTACH").is_some() {
                    eprintln!("[debug] EOT ignored; forcing stdin closed");
                }
                let _ = client.close_exec_stdin(&container_id, &exec_id).await;
            }
        }
    };

    if !eof_signalled {
        let _ = client.close_exec_stdin(&container_id, &exec_id).await;
    }
    let _ = client.delete_exec(&container_id, &exec_id).await;

    // Tell the output pump to drain and stop. It cannot detect this itself: we
    // hold a write end of the FIFO, so it would never see EOF and joining it
    // would hang — which is exactly what `attach` used to do on exit.
    output_stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = from_container.join();
    if let Some(handle) = errors_from_container {
        let _ = handle.join();
    }

    // Undo display modes the container's programs left on — and only those.
    // Emitted before the termios guard drops, so the terminal is put back in
    // one pass.
    if let Some(modes) = &modes {
        if let Ok(modes) = modes.lock() {
            let restore = modes.restore_sequence();
            if !restore.is_empty() {
                use std::io::Write;
                let _ = std::io::stdout().write_all(restore.as_bytes());
                let _ = std::io::stdout().flush();
            }
        }
    }

    // The stdin pump thread may still be blocked reading the user's terminal.
    // It is deliberately left alone: it holds nothing the process needs
    // released, and it is torn down when the process exits.
    let _ = std::fs::remove_dir_all(&io_dir);

    Ok(exit_code)
}

/// What a reconciliation sweep found and did.
#[derive(Debug, Default)]
pub struct ReconcileReport {
    /// Orphan mounts/loop devices released (mounted, or a backing file present,
    /// with no owning container record).
    pub released: Vec<String>,
    /// Orphans the helper reported releasing but the host still shows as
    /// mounted or loop-attached (F-77). Kept separate from `released` because
    /// merging them is how "nine released, zero bytes reclaimed" happened.
    pub not_released: Vec<String>,
    /// Orphan snapshots removed (a snapshot key with no matching container).
    pub snapshots_removed: Vec<String>,
    /// Backing files with no owning container record. **Reported, not deleted** —
    /// they may hold user data. Their mount and loop device are released, but the
    /// file is left for the operator to remove deliberately.
    pub orphan_backing_files: Vec<String>,
    /// Forwards reclaimed because a *stopped* project declares exactly them —
    /// ours by construction, since stop withdraws forwards (WP-M).
    pub stale_forwards: Vec<String>,
    /// Live forwards matching no project declaration. **Reported, never
    /// removed** (F-97): rootlesskit is shared with everything else the user
    /// runs, so a forward we cannot attribute may well be theirs, and deleting
    /// it would make a cleanup command destroy configuration it never created.
    pub unattributable_forwards: Vec<String>,
}

impl ReconcileReport {
    pub fn is_empty(&self) -> bool {
        self.released.is_empty()
            && self.not_released.is_empty()
            && self.snapshots_removed.is_empty()
            && self.orphan_backing_files.is_empty()
    }
}

/// Reclaim host resources whose owning container record is gone (#3/#10/#15/#23).
///
/// # Precedence rule (normative — recorded in SPEC.md Section 11, pending
/// promotion to a Section 3 subsection by the Product Owner)
///
/// **containerd's container records are the single source of truth for which
/// projects exist.** There is no side database. Any host resource — a mount, a
/// loop device, a snapshot — that is not owned by a current container record is
/// an orphan and is reclaimed. The one exception is a backing *file*, which may
/// hold user data: its mount and loop device are released, but the file itself
/// is only reported, never deleted, because destroying data is not something a
/// reconciliation sweep should do unprompted.
///
/// This is the backstop for the one window `delete`'s idempotent ordering cannot
/// cover: a crash after the container record is removed but before the volume is
/// released. Without it, that leaves a mounted, loop-attached volume nothing
/// references and nothing can find. Run it explicitly with `nemr reconcile`, or
/// periodically per `.claude/loop.md`.
pub async fn reconcile_orphans(client: &ContainerdClient) -> Result<ReconcileReport> {
    use std::collections::HashSet;

    let paths = VolumePaths::from_env()?;
    let containers = client.list_containers().await?;

    // The source of truth: names and ids of projects that actually exist.
    let known_names: HashSet<String> = containers
        .iter()
        .filter_map(|c| c.labels.get(LABEL_PROJECT).cloned())
        .collect();
    let known_ids: HashSet<&str> = containers.iter().map(|c| c.id.as_str()).collect();

    let mut report = ReconcileReport::default();
    let helper = HelperOps::new();

    // 1. Orphan mounts: a mount point under the managed dir whose name is not a
    //    known project. Release it (unmount + detach).
    if let Ok(entries) = std::fs::read_dir(paths.mount_dir()) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if known_names.contains(&name) || crate::engine::volume::validate_name(&name).is_err() {
                continue;
            }
            // F-77: deliberately NOT gated on `is_mounted`. A volume can be
            // unmounted and still have its loop device attached to a deleted
            // image — which is what an interrupted run leaves behind, and what
            // holds the disk: the kernel keeps the unlinked inode alive, so the
            // space is unreclaimable and `rm` on the image frees nothing while
            // reporting success. Gating on `is_mounted` skipped exactly the
            // state that needed reclaiming.
            // F-77: report what is TRUE afterwards, not that the call returned
            // Ok. The old code pushed to `released` on Ok and printed
            // "released ... (mount + loop device)" — while the helper had
            // detached nothing, because it could not find the loop device for a
            // deleted backing file. Nine volumes were reported released, zero
            // bytes were reclaimed, and the report was the only evidence anyone
            // had. Success asserted rather than observed is the defect class
            // this project keeps finding; a cleanup command is the last place it
            // should live.
            if let Err(error) = helper.unmount_and_detach(&name) {
                eprintln!("[nemr:reconcile] could not release orphan mount {name:?}: {error:#}");
                continue;
            }
            let still_mounted = crate::engine::volume::is_mounted(&paths.mount_point(&name));
            let still_attached =
                crate::engine::volume::attached_loop_device(&paths.image_file(&name));
            match (still_mounted, still_attached) {
                (false, None) => report.released.push(name),
                _ => {
                    eprintln!(
                        "[nemr:reconcile] {name:?} was NOT fully released: mounted={still_mounted}, \
                         loop={}. The helper reported success; the host disagrees. An attached \
                         loop device holds its (possibly deleted) image open, so this space is \
                         not reclaimed.",
                        still_attached
                            .map(|n| format!("/dev/loop{n}"))
                            .unwrap_or_else(|| "none".into())
                    );
                    report.not_released.push(name);
                }
            }
        }
    }

    // 2. Orphan backing files: an image with no container record. Release any
    //    stray mount/loop, but keep the file (it may hold data) and report it.
    if let Ok(entries) = std::fs::read_dir(paths.image_dir()) {
        for entry in entries.flatten() {
            let file_name = entry.file_name().to_string_lossy().into_owned();
            let Some(name) = file_name.strip_suffix(".img") else {
                continue;
            };
            if known_names.contains(name) || crate::engine::volume::validate_name(name).is_err() {
                continue;
            }
            let _ = helper.unmount_and_detach(name);
            report.orphan_backing_files.push(name.to_string());
        }
    }

    // 3. Stranded loop devices: attached to an image under the managed volume
    //    directory whose file AND mount point are both gone (F-79).
    //
    //    Steps 1 and 2 enumerate *directories* and *files*. A loop device whose
    //    backing image was deleted and whose mount point was removed appears in
    //    neither, so it was invisible to reconciliation while still pinning the
    //    unlinked inode — the disk stays full and nothing can find it. On the
    //    reference host that was 57 devices, with `nemr list` reporting all 57
    //    and `nemr reconcile` answering "nothing to reconcile": two commands
    //    disagreeing about the same host state, and the cleanup one giving the
    //    false all-clear.
    //
    //    /sys is the authoritative enumeration — it is what `untracked_volumes`
    //    already reads for `list`, so the two commands now share a source
    //    rather than each guessing from a different one.
    for name in untracked_volumes(client).await? {
        if report.released.contains(&name)
            || report.not_released.contains(&name)
            || report.orphan_backing_files.contains(&name)
        {
            continue;
        }
        if let Err(error) = helper.unmount_and_detach(&name) {
            eprintln!("[nemr:reconcile] could not release stranded volume {name:?}: {error:#}");
            report.not_released.push(name);
            continue;
        }
        if crate::engine::volume::attached_loop_device(&paths.image_file(&name)).is_none() {
            report.released.push(name);
        } else {
            eprintln!(
                "[nemr:reconcile] {name:?} is still loop-attached after release; its disk \
                 space is not reclaimed"
            );
            report.not_released.push(name);
        }
    }

    // 4. Orphan snapshots: an engine-created snapshot key with no container.
    for key in client.list_snapshot_keys().await? {
        if key.starts_with(config::CONTAINER_PREFIX) && !known_ids.contains(key.as_str()) {
            match client.remove_snapshot(&key).await {
                Ok(()) => report.snapshots_removed.push(key),
                Err(error) => {
                    eprintln!(
                        "[nemr:reconcile] could not remove orphan snapshot {key:?}: {error:#}"
                    )
                }
            }
        }
    }

    // Host port forwards (WP-M), with ownership PROVEN before anything is
    // removed (F-97). rootlesskit's forward table is shared with everything
    // else the user runs, and a forward they added by hand is indistinguishable
    // from one of our orphans — so the previous rule ("remove anything no
    // project declares") deleted the user's own configuration. A cleanup
    // command destroying what it did not create is the F-79 shape aimed at
    // something worse than a volume.
    if let Ok(live) = ports::list_live() {
        let mut declarations = Vec::new();
        for c in &containers {
            let Some(project) = c.labels.get(LABEL_PROJECT) else {
                continue;
            };
            let running = client
                .task_state(&c.id)
                .await
                .map(|s| s.is_running())
                .unwrap_or(false);
            for port in ports_from_labels(&c.labels) {
                declarations.push(ports::Declaration {
                    project: project.clone(),
                    port,
                    running,
                });
            }
        }

        for f in live {
            let label = format!("{}:{} -> {}", f.host_ip, f.host_port, f.container_port);
            match ports::classify_forward(&f, &declarations) {
                // Correct state: a running project's own forward. Nothing to say.
                ports::Disposition::Correct => {}
                // Ours by construction — stop withdraws forwards, so a live one
                // for a stopped project is our own leftover.
                ports::Disposition::Reclaim { project } => {
                    if ports::remove_live(f.id).is_ok() {
                        report
                            .stale_forwards
                            .push(format!("{label} (stopped project {project:?})"));
                    }
                }
                // Not ours as far as we can prove. Report it; never remove it.
                ports::Disposition::Unattributable => {
                    report
                        .unattributable_forwards
                        .push(format!("{label} (id {})", f.id));
                }
            }
        }
    }

    Ok(report)
}

/// Volume artifacts on disk that belong to no project (F-77).
///
/// `list` reports containers, because containerd is the source of truth for
/// what a project *is*. That is right, and it means an interrupted run's
/// leftovers — an image, a mount, a loop device with no container record — are
/// invisible. "Here are your projects" while 140 orphaned images fill the disk
/// is true and misleading, and the disk filling is not self-explanatory when it
/// happens.
///
/// Reported, never reclaimed here: `list` is a read-only command and an image
/// may hold data. `nemr reconcile` is the command that acts.
pub async fn untracked_volumes(client: &ContainerdClient) -> Result<Vec<String>> {
    use std::collections::HashSet;
    let paths = VolumePaths::from_env()?;
    let known: HashSet<String> = client
        .list_containers()
        .await?
        .iter()
        .filter_map(|c| c.labels.get(LABEL_PROJECT).cloned())
        .collect();

    let mut found = std::collections::BTreeSet::new();
    if let Ok(entries) = std::fs::read_dir(paths.image_dir()) {
        for entry in entries.flatten() {
            let raw = entry.file_name().to_string_lossy().into_owned();
            let Some(name) = raw.strip_suffix(".img") else {
                continue;
            };
            if !known.contains(name) && crate::engine::volume::validate_name(name).is_ok() {
                found.insert(name.to_string());
            }
        }
    }

    // Loop devices whose backing file is DELETED are invisible to the scan
    // above — there is no file left to list — and they are precisely the case
    // that fills a disk: the kernel holds the unlinked inode, so a
    // fully-allocated image occupies space that no `rm` can reclaim. Scanning
    // only the image directory would have reported "nothing untracked" on a
    // host with 3.5 GB stranded exactly this way.
    let prefix = paths.image_dir().to_string_lossy().into_owned();
    if let Ok(entries) = std::fs::read_dir("/sys/block") {
        for entry in entries.flatten() {
            let Ok(backing) = std::fs::read_to_string(entry.path().join("loop/backing_file"))
            else {
                continue;
            };
            let backing = backing.trim_end();
            let path = backing.strip_suffix(" (deleted)").unwrap_or(backing);
            let Some(rest) = path.strip_prefix(&prefix) else {
                continue;
            };
            let name = rest.trim_start_matches('/').trim_end_matches(".img");
            if !name.is_empty()
                && !known.contains(name)
                && crate::engine::volume::validate_name(name).is_ok()
            {
                found.insert(name.to_string());
            }
        }
    }
    Ok(found.into_iter().collect())
}

/// A project as reported by `list`.
#[derive(Debug, Clone)]
pub struct ProjectStatus {
    pub name: String,
    pub container_id: String,
    /// Quota recorded at creation time (the preset that was asked for).
    pub quota: String,
    pub running: bool,
    pub volume_path: String,
    /// The agent id recorded on the container label (WP-K: external tooling
    /// reads this from `nemr list --json` to describe a project without
    /// guessing).
    pub agent: String,
    /// Measured usage, absent when the volume is not currently mounted.
    pub usage: Option<crate::engine::volume::Usage>,
}

/// Everything about one project, in one place.
///
/// Answering "what state is this in?" meant reading `nemr list`, `df` and
/// `losetup` separately and correlating them by hand. Worse, the two questions
/// that actually cost time during the cross-machine work — is the mounted
/// filesystem the right one (F-28), and is there a credential — were not
/// answerable from any command at all.
#[derive(Debug, Clone)]
pub struct ProjectDetail {
    pub name: String,
    pub container_id: String,
    pub agent: Agent,
    pub running: bool,
    pub quota: String,
    pub mount_point: std::path::PathBuf,
    pub image_file: std::path::PathBuf,
    pub image_present: bool,
    pub mounted: bool,
    /// The loop device backing the mount, when it is loop-backed.
    pub loop_device: Option<u32>,
    /// The image the mounted filesystem is actually backed by (F-28). `Some`
    /// and equal to `image_file` is healthy; anything else is not.
    pub mounted_image: Option<std::path::PathBuf>,
    pub usage: Option<crate::engine::volume::Usage>,
    /// Base image reference and the digest present on this host, if any.
    pub base_image: String,
    pub base_image_digest: Option<String>,
    /// Host credential (AUTH-02), and when it was last written.
    pub credential: Option<std::path::PathBuf>,
    pub credential_modified: Option<std::time::SystemTime>,
    /// When the credential's OAuth token expires (unix seconds), if the file
    /// carries one. Presence is not validity: the three-login loop that
    /// motivated this was a *present*, read-only, *expired* credential, and
    /// "last written 0 days ago" was true and useless (the F-56 shape).
    pub credential_expires_at: Option<i64>,
    /// The refresh token's expiry, if dated. Under D-02's (f) this, not the
    /// access token, is what decides whether a session can recover.
    pub credential_refresh_expires_at: Option<i64>,
    /// Claude Code blanked the file after a dead refresh.
    pub credential_blank: bool,
    /// E-21: a credential file that is not the engine's placeholder. False
    /// on a machine that has never logged in.
    pub credential_present: bool,
    /// A running session still sees a file the host has since replaced
    /// (F-12); `None` when not running or unreadable.
    pub credential_stale: Option<bool>,
}

impl ProjectDetail {
    /// Whether the mounted filesystem is this project's own volume (F-28).
    ///
    /// `None` when nothing is mounted — an absent volume is not a wrong one,
    /// and reporting them the same way is how "not started yet" gets mistaken
    /// for corruption.
    pub fn mount_is_correct(&self) -> Option<bool> {
        if !self.mounted {
            return None;
        }
        Some(self.mounted_image.as_deref() == Some(self.image_file.as_path()))
    }
}

/// Change a project's agent after creation (E-15).
///
/// Updates the recorded label so `start`/`attach` launch the new agent's CLI,
/// and returns the (previous, new) pair so the caller can be blunt about what
/// this does NOT do: the old conversation stays on disk but the new agent will
/// not see it, because each CLI stores history in its own format. One agent at
/// a time per project is the honest model — this switches which one, it does
/// not merge two histories, and it is the future home of cross-agent migration
/// (a post-daemon epic) without the command changing.
pub async fn set_agent(
    client: &ContainerdClient,
    name: &str,
    agent: Agent,
) -> crate::error::Result<(Agent, Agent)> {
    use crate::error::Error;

    let container_id = config::container_id(name);
    let container = client
        .list_containers()
        .await
        .map_err(Error::Internal)?
        .into_iter()
        .find(|c| c.id == container_id)
        .ok_or_else(|| Error::NoSuchProject {
            name: name.to_string(),
        })?;

    if client
        .task_state(&container_id)
        .await
        .map_err(Error::Internal)?
        .is_running()
    {
        return Err(Error::WrongState {
            name: name.to_string(),
            state: "running; stop it before switching agents so nothing is mid-session",
        });
    }

    let previous = agent_from_labels(&container.labels);
    if previous == agent {
        return Ok((previous, agent));
    }

    let mut labels = container.labels.clone();
    labels.insert(LABEL_AGENT.to_string(), agent.id().to_string());
    client
        .update_container_labels(&container_id, labels)
        .await
        .map_err(Error::Internal)?;

    Ok((previous, agent))
}

/// Re-bind the host's current credential into a running session if it is
/// stale (F-12). Returns `Ok(true)` if a re-bind was performed, `Ok(false)` if
/// the session already sees the host's file, is not running, or there is no
/// host credential to bind. The check is a read (device + inode through
/// `/proc/<pid>/root`); the repair runs in a single-threaded child of the
/// daemon binary (`credential_bind`).
pub async fn rebind_credential(client: &ContainerdClient, name: &str) -> Result<bool> {
    let container_id = resolve(client, name).await?;
    if !client.task_state(&container_id).await?.is_running() {
        return Ok(false);
    }
    let Some(pid) = client.task_pid(&container_id).await? else {
        return Ok(false);
    };
    let host = auth::host_credentials_path()?;
    if !host.exists() {
        return Ok(false);
    }
    let container = std::path::Path::new(config::CONTAINER_CREDENTIALS);
    if auth::credential_is_stale(pid, container, &host) != Some(true) {
        return Ok(false);
    }
    let host_for_child = host.clone();
    let container_for_child = container.to_path_buf();
    tokio::task::spawn_blocking(move || {
        crate::engine::credential_bind::rebind_in_child(pid, &host_for_child, &container_for_child)
    })
    .await
    .context("re-bind task")??;
    // Fail closed: the child's exit status is not the property. What the task
    // sees is — read it again rather than trusting the report.
    if auth::credential_is_stale(pid, container, &host) != Some(false) {
        bail!(
            "the re-bind reported success but the session still does not see the host's \
             current credential (task {pid}); leave it to: nemr stop {name} && nemr start {name}"
        );
    }
    Ok(true)
}

/// Seed the session's `.claude.json` from the host's, by allowlist, if the
/// session has none yet (F-131). Returns whether the seed was written — the
/// exec's own report, read back: the guarded write prints nothing, so the
/// presence of the file afterwards is what is checked.
pub async fn seed_session_config(client: &ContainerdClient, name: &str) -> Result<bool> {
    let host_config = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .map(|h| h.join(".claude.json"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
    let seed = crate::engine::seed::session_config_seed(host_config.as_ref());
    let (before, _) = exec_capture(
        client,
        name,
        &["/bin/sh", "-c", "test -e /root/.claude.json"],
    )
    .await?;
    if before == 0 {
        return Ok(false);
    }
    let argv = crate::engine::seed::seed_write_argv(&seed);
    let argv_ref: Vec<&str> = argv.iter().map(String::as_str).collect();
    let (exit, out) = exec_capture(client, name, &argv_ref).await?;
    if exit != 0 {
        bail!(
            "writing the seed inside the session failed (exit {exit}): {}",
            out.trim()
        );
    }
    Ok(true)
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Gather everything `nemr status` reports.
pub async fn status(client: &ContainerdClient, name: &str) -> crate::error::Result<ProjectDetail> {
    use crate::error::Error;

    let container_id = config::container_id(name);
    let container = client
        .list_containers()
        .await
        .map_err(Error::Internal)?
        .into_iter()
        .find(|c| c.id == container_id)
        .ok_or_else(|| Error::NoSuchProject {
            name: name.to_string(),
        })?;

    let paths = VolumePaths::from_env().map_err(Error::Internal)?;
    let mount_point = paths.mount_point(name);
    let image_file = paths.image_file(name);
    let mounted = crate::engine::volume::is_mounted(&mount_point);

    let credential = auth::host_credentials_path().ok().filter(|p| p.exists());
    // F-10: a login that landed beside a leftover marker is cleaned on first
    // detection, so the file is what Claude Code alone would have written.
    if let Some(p) = &credential {
        match auth::scrub_placeholder_marker(p) {
            Ok(true) => tracing::info!("[nemr] cleared the engine's placeholder marker from {} — a login landed beside it (F-10)", p.display()),
            Ok(false) => {}
            Err(e) => tracing::warn!("[nemr] could not clear the placeholder marker from {}: {e:#}", p.display()),
        }
    }
    // E-21: present means a file that is not the engine's placeholder — the
    // page and `status` say "no login yet" from this, not from a verdict.
    let credential_present = credential
        .as_ref()
        .map(|p| !auth::is_placeholder(&std::fs::read_to_string(p).unwrap_or_default()))
        .unwrap_or(false);
    let credential_modified = credential
        .as_ref()
        .and_then(|p| std::fs::metadata(p).ok())
        .and_then(|m| m.modified().ok());
    let facts = credential
        .as_ref()
        .map(|p| auth::credential_facts_at(p))
        .unwrap_or_else(|| auth::credential_facts(""));
    let running = client
        .task_state(&container_id)
        .await
        .map_err(Error::Internal)?
        .is_running();
    // F-12, read not repaired: what the running task sees versus what the host
    // has now. Only meaningful while a task exists.
    let credential_stale = match (&credential, running) {
        (Some(host), true) => client
            .task_pid(&container_id)
            .await
            .ok()
            .flatten()
            .and_then(|pid| {
                auth::credential_is_stale(
                    pid,
                    std::path::Path::new(config::CONTAINER_CREDENTIALS),
                    host,
                )
            }),
        _ => None,
    };

    Ok(ProjectDetail {
        name: name.to_string(),
        agent: agent_from_labels(&container.labels),
        running,
        container_id,
        quota: container
            .labels
            .get(LABEL_SIZE)
            .cloned()
            .unwrap_or_else(|| "unknown".into()),
        loop_device: crate::engine::volume::attached_loop_device(&image_file),
        mounted_image: crate::engine::volume::mounted_image_path(&mount_point),
        usage: if mounted {
            crate::engine::volume::usage(&mount_point)
        } else {
            None
        },
        image_present: image_file.exists(),
        base_image_digest: client.image_target_digest(config::BASE_IMAGE).await.ok(),
        base_image: config::BASE_IMAGE.to_string(),
        mount_point,
        image_file,
        mounted,
        credential,
        credential_modified,
        credential_expires_at: facts.access.expires_at_secs(),
        credential_refresh_expires_at: facts.refresh.expires_at_secs(),
        credential_blank: facts.blank,
        credential_present,
        credential_stale,
    })
}

/// List all projects (Milestone 6).
///
/// State comes from containerd: the container records and their labels are the
/// source of truth, and usage is measured from the mounted filesystem. There is
/// no engine-side database to fall out of step with reality — which is what
/// AC-6.1 is really testing when it cross-checks against `ctr`.
pub async fn list(client: &ContainerdClient) -> Result<Vec<ProjectStatus>> {
    let containers = client.list_containers().await?;
    let mut projects = Vec::new();

    for container in containers {
        // Only containers this engine created are projects. Others in the
        // namespace are none of our business, and reporting them would make
        // `list` disagree with reality in the other direction.
        let Some(name) = container.labels.get(LABEL_PROJECT) else {
            continue;
        };

        let running = client.task_state(&container.id).await?.is_running();
        let volume_path = container
            .labels
            .get(LABEL_VOLUME)
            .cloned()
            .unwrap_or_default();
        let usage = if volume_path.is_empty() {
            None
        } else {
            crate::engine::volume::usage(std::path::Path::new(&volume_path))
        };

        projects.push(ProjectStatus {
            name: name.clone(),
            container_id: container.id.clone(),
            quota: container
                .labels
                .get(LABEL_SIZE)
                .cloned()
                .unwrap_or_else(|| "unknown".into()),
            running,
            volume_path,
            agent: agent_from_labels(&container.labels).id().to_string(),
            usage,
        });
    }

    projects.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(projects)
}

/// Delete a project and everything it owns (Milestone 6).
///
/// # Ordering: the container record is the anchor, removed last (#3/#10/#20)
///
/// The container record is what `list` and `resolve` use to find a project —
/// there is no side database. So it is removed **last**, only once everything it
/// owns is already gone. The earlier order removed it second, before releasing
/// the volume, which meant a helper failure at the release step stranded a
/// mounted, loop-attached volume that `list` could no longer see, `delete` could
/// no longer resolve to retry, and `create` of the same name rejected with a
/// confusing "volume exists but no container" message. Releasing first is safe
/// because the task is already stopped, so nothing is using the mount; the
/// record referencing the volume path is only metadata.
///
/// Every step is idempotent, so a `delete` interrupted partway is completed by
/// simply running it again: `stop_task` tolerates a missing task, the helper's
/// unmount tolerates an already-released or already-gone volume, and the file
/// removals tolerate absence. The startup reconciliation sweep
/// ([`reconcile_orphans`]) is the backstop for the one window this cannot cover
/// itself — a crash after the record is gone but before the volume is released.
///
/// Confirmation is the caller's responsibility (AC-6.2) — this function does
/// the deleting, the CLI does the asking, so a scripted caller is not fighting
/// a prompt.
pub async fn delete(client: &ContainerdClient, name: &str) -> Result<()> {
    let container_id = resolve(client, name).await?;
    let paths = VolumePaths::from_env()?;

    // Release this project's host ports first (WP-M). They live in rootlesskit,
    // not in the container, so removing the container alone leaves them bound
    // to nothing — a host port held hostage by a project that no longer exists,
    // and a collision message that would name a project the user cannot find.
    withdraw_declared_ports(client, name).await;

    // NET-02: and the session network. delete calls stop_task directly rather
    // than going through stop(), so it needs its own teardown — without this
    // the veth and the NAT rule outlived every deleted project, which the
    // cleanup check caught.
    let session_network = find_container(client, name)
        .await
        .ok()
        .and_then(|c| allocation_from_labels(&c.labels));

    // 1. Stop the task. Idempotent: stop_task returns NoTask if none is running.
    client.stop_task(&container_id).await?;

    if let Some(alloc) = session_network {
        let _ = tokio::task::spawn_blocking(move || netns::disconnect_session(alloc)).await;
    }

    // 2. Release the volume (unmount + detach) BEFORE the record, so a failure
    //    here leaves the project still listable and this delete retryable.
    HelperOps::new()
        .unmount_and_detach(name)
        .with_context(|| format!("failed to release the volume for project {name:?}"))?;

    // 3. Backing file and mount point, now that the loop device is detached.
    let image = paths.image_file(name);
    if image.exists() {
        std::fs::remove_file(&image)
            .with_context(|| format!("failed to remove {}", image.display()))?;
    }
    let mount_point = paths.mount_point(name);
    let _ = std::fs::remove_dir(&mount_point);

    // 4. The container record and its snapshot, LAST. Until this returns, the
    //    project is still discoverable and every prior step is safe to repeat.
    client.delete_container(&container_id).await?;

    Ok(())
}

/// Export a project to a bundle (M9).
///
/// The project must be stopped, so the volume is not being written while it is
/// read. Exporting a running project would capture a transcript mid-append —
/// a torn read that produces a bundle which looks fine and restores a corrupt
/// session, which is the failure shape this project keeps hitting.
pub async fn export(
    client: &ContainerdClient,
    name: &str,
    destination: &std::path::Path,
    policy: crate::bundle::policy::Policy,
) -> crate::error::Result<crate::bundle::export::ExportSummary> {
    use crate::bundle::export::{export as write_bundle, ExportRequest};
    use crate::error::Error;

    let container_id = config::container_id(name);
    if !client
        .container_exists(&container_id)
        .await
        .map_err(Error::Internal)?
    {
        return Err(Error::NoSuchProject {
            name: name.to_string(),
        });
    }

    if client
        .task_state(&container_id)
        .await
        .map_err(Error::Internal)?
        .is_running()
    {
        return Err(Error::WrongState {
            name: name.to_string(),
            state: "running; stop it first so the volume is not written while it is read",
        });
    }

    // VOL-06: the volume must actually be mounted, or we would export whatever
    // the mount point happens to sit on — the host filesystem.
    ensure_volume_mounted(name).map_err(Error::Internal)?;

    let paths = VolumePaths::from_env().map_err(Error::Internal)?;
    let mount_point = paths.mount_point(name);

    let quota = read_recorded_size(&paths, name)
        .map(|size| size.to_string())
        .unwrap_or_else(|| "unknown".to_string());

    // The producing agent comes from the container's own label (E-15). A
    // project predating the field reads back as the default, which is correct.
    let labels = client
        .list_containers()
        .await
        .map_err(Error::Internal)?
        .into_iter()
        .find(|c| c.id == container_id)
        .map(|c| c.labels)
        .unwrap_or_default();
    let agent = agent_from_labels(&labels);

    // The base image is referenced by digest, never carried (D-06) — and its
    // identity comes from the CONTAINER, not from this engine's constant.
    let base_image = base_image_identity(client, &container_id)
        .await
        .map_err(Error::Internal)?;

    // Declared packages (F-118): detect what the owner installed and write the
    // list onto the volume BEFORE the walk, so it travels like any other
    // session state. Detection reads the stopped container's snapshot — the
    // project is stopped (checked above) and the read is 0.01–0.04s.
    //
    // Failure here is a WARNING, not a refusal: the export's job is carrying
    // the session, and a session without its package list is degraded, while a
    // session that cannot leave the machine is lost. The warning is tagged so
    // it reaches the user (F-98), and the acceptance covers the happy path.
    match refresh_declared_packages(client, &container_id, &mount_point).await {
        Ok(Some(count)) => {
            tracing::info!("[nemr] {name}: {count} declared package(s) recorded in the bundle");
        }
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(
                nemr_audit = "warning",
                "[nemr] {name}: could not record declared packages; the bundle will carry \
                 the previous list if one exists ({error:#})"
            );
        }
    }

    let request = ExportRequest {
        project: name,
        agent: agent.id(),
        quota: &quota,
        source_root: &mount_point,
        base_image,
        policy,
    };
    write_bundle(&request, destination)
}

/// Import a bundle into a new project (M10).
///
/// The destination project must already exist and be stopped: creating it is a
/// separate step so the user chooses the quota, and a quota too small for the
/// bundle is refused up front rather than discovered mid-extraction.
///
/// Per D-02 the bundle carries no credential. The caller authenticates on the
/// destination host before attaching; `import` states this rather than leaving
/// it to be discovered at the first API call.
/// Restore a bundle, creating the destination project if it does not exist.
///
/// # Why this exists
///
/// Restoring onto a fresh host used to be three commands, one of which required
/// inventing a number:
///
/// ```text
/// nemr create htmltest --size 500MB    # a quota the user had to guess
/// nemr import htmltest bundle.nemr
/// nemr start htmltest
/// ```
///
/// The bundle already records the source project's name and quota, so the guess
/// was being demanded for information the file carried. That is the restore
/// flow — the point of the product — and it required knowing internals.
///
/// `name` and `size` override the manifest when the destination host needs
/// different ones; both default to what the bundle says. **No bundle-format
/// change was needed:** `ProjectInfo` has carried `name` and `quota` since v1.
pub async fn import_creating(
    client: &ContainerdClient,
    bundle_path: &std::path::Path,
    name: Option<&str>,
    size: Option<VolumeSize>,
) -> crate::error::Result<(String, crate::bundle::import::ExtractSummary)> {
    use crate::bundle::import::open as open_bundle;
    use crate::error::Error;

    let bundle = open_bundle(bundle_path)?;
    let manifest = &bundle.manifest;
    let name = name.unwrap_or(&manifest.project.name).to_string();
    // E-15: the imported project runs the agent that produced the bundle, so
    // the right CLI launches on the far machine. An unrecognised agent (a
    // bundle from a newer nemr) is refused rather than silently defaulted —
    // launching the wrong CLI against a session is worse than a clear failure.
    let agent = manifest
        .project
        .agent
        .parse::<Agent>()
        .map_err(|_| Error::UnknownAgent {
            requested: manifest.project.agent.clone(),
            known: Agent::all()
                .iter()
                .map(|a| a.id())
                .collect::<Vec<_>>()
                .join(", "),
        })?;
    crate::engine::volume::validate_name(&name)?;

    let size = match size {
        Some(size) => size,
        None => manifest.project.quota.parse::<VolumeSize>().map_err(|e| {
            Error::Internal(e.context(format!(
                "the bundle records quota {:?}, which this build does not recognise. \
                 Pass --size to choose one explicitly.",
                manifest.project.quota
            )))
        })?,
    };

    // Refuse rather than clobber. An import that silently merged into an
    // existing project would overwrite a session with another one, and the
    // damage is not visible until someone opens it.
    let container_id = config::container_id(&name);
    let exists = client
        .container_exists(&container_id)
        .await
        .map_err(Error::Internal)?;
    if exists {
        return Err(Error::RestoreTargetExists {
            other: format!("{name}-restored"),
            bundle: bundle_path.display().to_string(),
            name,
        });
    }

    create_with_auth(client, &name, size, agent)
        .await
        .map_err(Error::Internal)?;

    // A failed restore must not leave a half-populated project behind: the user
    // asked for a session, and an empty project wearing its name is worse than
    // nothing, because the name is then taken.
    match import(client, &name, bundle_path).await {
        Ok(summary) => Ok((name, summary)),
        Err(error) => {
            if let Err(cleanup) = delete(client, &name).await {
                eprintln!(
                    "[nemr:import] restore failed AND the partially-created project {name:?} \
                     could not be removed: {cleanup:#}. Remove it with `nemr delete {name}`."
                );
            }
            Err(error)
        }
    }
}

pub async fn import(
    client: &ContainerdClient,
    name: &str,
    bundle_path: &std::path::Path,
) -> crate::error::Result<crate::bundle::import::ExtractSummary> {
    use crate::bundle::import::{open as open_bundle, ImportChecks};
    use crate::error::Error;

    let container_id = config::container_id(name);
    if !client
        .container_exists(&container_id)
        .await
        .map_err(Error::Internal)?
    {
        return Err(Error::NoSuchProject {
            name: name.to_string(),
        });
    }
    if client
        .task_state(&container_id)
        .await
        .map_err(Error::Internal)?
        .is_running()
    {
        return Err(Error::WrongState {
            name: name.to_string(),
            state: "running; stop it before importing over its volume",
        });
    }

    // Read and validate the bundle before touching the destination.
    let bundle = open_bundle(bundle_path)?;

    ensure_volume_mounted(name).map_err(Error::Internal)?;
    let paths = VolumePaths::from_env().map_err(Error::Internal)?;
    let mount_point = paths.mount_point(name);

    // The destination's real usable capacity, measured rather than assumed from
    // the preset — ext4 metadata means the usable total is below the request.
    let usage = crate::engine::volume::usage(&mount_point).ok_or_else(|| {
        Error::host(
            "the destination volume",
            format!("{} is not mounted", mount_point.display()),
            "Start the project once so its volume is mounted, then retry.",
        )
    })?;
    let quota = read_recorded_size(&paths, name)
        .map(|size| size.to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let base_image = resolve_base_image(
        client,
        bundle.base_image_digest(),
        bundle.base_image_reference(),
        bundle.base_image_chain_id(),
    )
    .await;
    bundle.check(&ImportChecks {
        base_image,
        destination_capacity: usage.available,
        destination_quota: &quota,
    })?;

    bundle.extract(&mount_point)
}

/// Find the base image a bundle needs, **locally only** (D-08 parts 1 and 2).
///
/// Public so the message a user actually sees can be asserted in a test rather
/// than a reconstruction of it — the error text is the deliverable here, and a
/// test that builds its own input would only be checking `format!`.
///
/// Two attempts, in this order:
///
///   1. the configured reference, by name — the fast path, one gRPC call;
///   2. every local image, by digest — because the same bytes filed under a
///      different tag are still the right image, and a host that already has
///      them must not be sent to a registry.
///
/// Only after both miss does the registry become relevant. Each attempt is
/// recorded so the error can say where it looked rather than just that it
/// failed; see `Error::BaseImageUnresolved`.
/// What this project actually runs on (F-115).
///
/// Every field used to come from `config::BASE_IMAGE`, so a bundle recorded what
/// the EXPORTING ENGINE builds today rather than what the project was built on.
/// Usually the same, and silently wrong when it differs — which is precisely the
/// substitution the manifest calls "a silent-wrong-result defect" and claims to
/// prevent. On the developer host three of four projects name an image their
/// rootfs is not, and one names a registry namespace nobody owns (F-25).
///
/// The chain id is read from the snapshot and is always true. The reference and
/// digest are filled from the image that chain id identifies; when no local
/// image does, the digest is left EMPTY rather than guessed — an unproven
/// identity recorded as a fact is the bug being fixed — and the reference falls
/// back to this engine's own constant purely as a hint, never to the container's
/// recorded reference, which may name a namespace we do not own.
async fn base_image_identity(
    client: &ContainerdClient,
    container_id: &str,
) -> anyhow::Result<crate::bundle::manifest::BaseImageRef> {
    use crate::bundle::manifest::BaseImageRef;

    // The snapshot key IS the container id: create_container prepares it that
    // way, which is why the parent equals the image's chain id.
    let chain_id = client.snapshot_parent(container_id).await?;

    match client.image_with_chain_id(&chain_id).await? {
        Some(image) => Ok(BaseImageRef {
            reference: image.name,
            digest: image.digest,
            rootfs_chain_id: chain_id,
        }),
        None => Ok(BaseImageRef {
            reference: config::BASE_IMAGE.to_string(),
            digest: String::new(),
            rootfs_chain_id: chain_id,
        }),
    }
}

pub async fn resolve_base_image(
    client: &ContainerdClient,
    wanted_digest: &str,
    wanted_reference: &str,
    wanted_chain_id: &str,
) -> crate::bundle::import::BaseImageResolution {
    use crate::bundle::import::BaseImageResolution;
    let mut where_looked = Vec::new();

    // The chain id, when the bundle carries one, is the whole answer: it names
    // the rootfs itself rather than a tag or a manifest digest that a retag can
    // move. Deliberately NOT falling through to the digest checks when it fails
    // to match — a digest match on a different rootfs is exactly the
    // substitution this refuses to make.
    if !wanted_chain_id.is_empty() {
        match client.image_with_chain_id(wanted_chain_id).await {
            Ok(Some(image)) => {
                return BaseImageResolution::Present {
                    reference: image.name,
                }
            }
            Ok(None) => where_looked.push(format!(
                "local containerd, by rootfs chain id {wanted_chain_id}: no local image \
                 builds that rootfs"
            )),
            Err(error) => where_looked.push(format!(
                "local containerd, by rootfs chain id {wanted_chain_id}: the query failed \
                 ({error})"
            )),
        }
        where_looked
            .push("no registry: nemr does not fetch images itself (D-08 part 1)".to_string());
        return BaseImageResolution::Unresolved {
            where_looked,
            advice: format!(
                "This bundle records the rootfs it was built on, and no image here builds \
                 it. Pull the base image this bundle names into the same rootless \
                 containerd:\n\n    \
                 CONTAINERD_ADDRESS={socket} \\\n      \
                 ctr --namespace {namespace} images pull {reference}\n\n\
                 If that image is not the right one, its rootfs will not match and the \
                 import will still refuse — which is the point.",
                socket = ContainerdClient::default_socket_path()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|_| "$XDG_RUNTIME_DIR/containerd/containerd.sock".to_string()),
                namespace = client.namespace(),
                reference = wanted_reference,
            ),
        };
    }

    match client.image_target_digest(config::BASE_IMAGE).await {
        Ok(digest) if digest == wanted_digest => {
            return BaseImageResolution::Present {
                reference: config::BASE_IMAGE.to_string(),
            }
        }
        Ok(digest) => where_looked.push(format!(
            "local containerd, by name {}: present, but its digest is {digest}",
            config::BASE_IMAGE
        )),
        Err(_) => where_looked.push(format!(
            "local containerd, by name {}: not present",
            config::BASE_IMAGE
        )),
    }

    match client.images_with_digest(wanted_digest).await {
        Ok(matches) if !matches.is_empty() => {
            return BaseImageResolution::Present {
                reference: matches[0].name.clone(),
            }
        }
        Ok(_) => {
            where_looked.push("local containerd, by digest across all images: no match".to_string())
        }
        Err(error) => where_looked.push(format!(
            "local containerd, by digest across all images: the query failed ({error})"
        )),
    }

    // D-08 part 1, as ruled: nemr does not pull. Saying so explicitly is the
    // point — an error that implied a network attempt it never made would send
    // someone to debug their connection, and an error claiming to know whether a
    // registry was reachable would be inventing a fact.
    where_looked.push("no registry: nemr does not fetch images itself (D-08 part 1)".to_string());

    BaseImageResolution::Unresolved {
        where_looked,
        advice: format!(
            "nemr does not fetch images. Pull it into the same rootless containerd \
             this engine uses:\n\n    \
             CONTAINERD_ADDRESS={socket} \\\n      \
             ctr --namespace {namespace} images pull {reference}\n\n\
             Then retry the import. If the image is not published anywhere, build it \
             locally: ./scripts/build_base_image.sh",
            socket = ContainerdClient::default_socket_path()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|_| "$XDG_RUNTIME_DIR/containerd/containerd.sock".to_string()),
            namespace = client.namespace(),
            reference = wanted_reference,
        ),
    }
}

// --- port forwarding (WP-M) -------------------------------------------------

use crate::engine::netns;
use crate::engine::ports;

/// The session network a project was allocated, from its label.
pub fn allocation_from_labels(
    labels: &std::collections::HashMap<String, String>,
) -> Option<netns::Allocation> {
    labels
        .get(LABEL_NETNS)
        .and_then(|r| netns::decode_allocation(r))
}

/// Every session network index currently spoken for.
///
/// The error is propagated, not swallowed. `unwrap_or_default()` here turned a
/// containerd read failure into "nothing is allocated", which hands the next
/// project index 0 on top of a live one — and a duplicate index does not error,
/// it silently points one project's host forwards at another project's session.
/// Refusing to create is the cheap failure; the quiet one costs a debugging
/// session that starts nowhere near this line.
async fn allocated_indices(client: &ContainerdClient) -> Result<Vec<u8>> {
    let containers = client
        .list_containers()
        .await
        .context("reading the projects that already hold a session network")?;
    Ok(containers
        .iter()
        .filter(|c| c.labels.contains_key(LABEL_PROJECT))
        .filter_map(|c| allocation_from_labels(&c.labels))
        .map(|a| a.index)
        .collect())
}

/// Allocate this project's session network, or return the one it already has.
///
/// Serialised process-wide. Allocation is a read-modify-write — read the set in
/// use, pick a free index, publish it on the container record — and the daemon
/// answers RPCs concurrently, so two `nemr create` calls could both read the set
/// without the other's project in it and both take the same index. Nothing
/// downstream detects the duplicate: the two projects derive the same /24, the
/// same addresses and the same link name, and the loser starts with no network
/// while its host ports forward into the winner's session.
///
/// The lock is process-wide, which is exactly as strong as the daemon's own
/// claim to be the single writer to containerd (see src/daemon/mod.rs). If that
/// ever stops being true, this needs to become a lock in containerd itself —
/// recorded as F-99.
static NETWORK_ALLOCATION: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Pick a free index. The caller must hold [`NETWORK_ALLOCATION`] and must not
/// release it until the choice is published on a container record — an index
/// nobody can see is an index the next caller will pick too.
async fn allocate_index_locked(client: &ContainerdClient) -> Result<netns::Allocation> {
    // Refuse before recording anything if the host already routes our range —
    // allocating into a conflict does not error, it silently misroutes, which is
    // the worst failure in this area.
    blocking(netns::check_range_is_free).await?;
    netns::allocate_index(&allocated_indices(client).await?)
}

/// Run one of `engine::netns`'s synchronous host commands off the async
/// workers.
///
/// Everything in `netns` shells out — `nsenter`, `ip`, `iptables`, `sysctl` —
/// through `std::process::Command::output()`, which blocks the thread it runs
/// on. Called straight from an async handler that pins a tokio worker for the
/// duration: measured at 0.49–0.95s per call, against four workers on a CI
/// runner. Enough concurrent sessions and the daemon stops answering unrelated
/// RPCs, which surfaces as a command that failed for no reason it can name.
async fn blocking<T, F>(f: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .context("a host networking command could not be run")?
}

/// Choose and record a session network for an existing project that has none.
///
/// Used by `start` for a project that predates NET-02: those carry no
/// allocation, and their task now gets an empty namespace whether or not
/// anything wires it, so "no label" cannot mean "no networking wanted".
async fn allocate_and_record(
    client: &ContainerdClient,
    container: &crate::containerd::containers::ContainerSummary,
) -> Result<netns::Allocation> {
    let _guard = NETWORK_ALLOCATION.lock().await;

    // Re-read under the lock: another start may have allocated for this same
    // project while we were waiting, and taking a second index would leave the
    // first one recorded nowhere and reserved forever.
    let fresh = find_container_by_id(client, &container.id).await?;
    if let Some(existing) = allocation_from_labels(&fresh.labels) {
        return Ok(existing);
    }

    let alloc = allocate_index_locked(client).await?;
    let mut labels = fresh.labels.clone();
    labels.insert(LABEL_NETNS.to_string(), netns::encode_allocation(alloc));
    client
        .update_container_labels(&fresh.id, labels)
        .await
        .with_context(|| {
            format!(
                "recording the session network {} on {:?}",
                alloc.cidr(),
                fresh.id
            )
        })?;
    Ok(alloc)
}

/// Wire a freshly started task into its session network (NET-02).
///
/// Separated from `start` so every failure below leaves through one place, and
/// one place decides what to do about the task that is already running.
async fn wire_session(client: &ContainerdClient, name: &str, pid: u32) -> Result<()> {
    // The control on the migration above. If the task is in rootlesskit's own
    // namespace, wiring would put this session's addresses on rootlesskit's
    // interfaces and collide with its default route — so refuse, and say which
    // of the two situations this is. Reported rather than assumed: the inode
    // comparison is the same evidence NET-01 and NET-02 both rest on.
    if netns::shares_rootlesskit_namespace(pid)
        .context("checking whether the task received its own network namespace")?
    {
        bail!(
            "{name:?} started in rootlesskit's shared network namespace rather than its own, \
             so it cannot be given a session network.\n\
             Its container record asks for no network namespace and the migration that adds \
             one did not take effect. Recreate the project, or report this — wiring a session \
             into the shared namespace would put its addresses on rootlesskit's interfaces."
        );
    }

    let container = find_container(client, name).await?;
    let alloc = match allocation_from_labels(&container.labels) {
        Some(alloc) => alloc,
        // A project created before NET-02. Allocate and record one now, once.
        None => allocate_and_record(client, &container).await?,
    };
    blocking(move || netns::connect_session(pid, alloc)).await
}

/// The container record for a project, or NoSuchProject.
async fn find_container(
    client: &ContainerdClient,
    name: &str,
) -> crate::error::Result<crate::containerd::containers::ContainerSummary> {
    let container_id = config::container_id(name);
    client
        .list_containers()
        .await
        .map_err(crate::error::Error::Internal)?
        .into_iter()
        .find(|c| c.id == container_id)
        .ok_or_else(|| crate::error::Error::NoSuchProject {
            name: name.to_string(),
        })
}

/// Install a project's declared packages (F-118), verifying outcomes.
///
/// Explicit and on demand — never run by `start` or `import` (Product Owner
/// ruling: nothing in this CLI does work you did not ask for, and both those
/// paths sit inside tested no-network guarantees this must not spend). The
/// project must be RUNNING: installation happens inside the session, where the
/// network and the dpkg database are the session's own.
///
/// Per package, not one apt invocation: a batch install fails as a unit, and
/// "7 of 8 installed, xsv is gone from the archive" is a report the owner can
/// act on, while "the batch failed" is not. Each verdict is read from dpkg's
/// state, never from apt's exit status (F-122).
pub async fn provision(
    client: &ContainerdClient,
    name: &str,
) -> Result<crate::engine::packages::ProvisionReport> {
    use crate::engine::packages;

    let paths = VolumePaths::from_env()?;
    ensure_volume_mounted(name)?;
    let mount_point = paths.mount_point(name);

    let declared = packages::read_declared(&mount_point)?
        .map(|list| list.packages)
        .unwrap_or_default();
    if declared.is_empty() {
        return Ok(packages::ProvisionReport {
            installed: Vec::new(),
            failed: Vec::new(),
        });
    }

    let mut report = packages::ProvisionReport {
        installed: Vec::new(),
        failed: Vec::new(),
    };
    for package in &declared {
        let script = packages::provision_script(package);
        let (_, output) = exec_capture(client, name, &["/bin/sh", "-c", &script]).await?;
        let (ok, why) = packages::parse_verdict(&output);
        if ok {
            report.installed.push(package.clone());
        } else {
            report.failed.push((package.clone(), why));
        }
    }
    Ok(report)
}

/// The declared packages a running project is missing, for `start`'s warning.
///
/// Read from inside the session (dpkg-query), because the question is what the
/// SESSION has, and it must never fail the start: any error reads as "cannot
/// tell", reported as such by the caller rather than swallowed into "none
/// missing" — a broken check must not look like a healthy session (F-109).
pub async fn missing_declared_packages(
    client: &ContainerdClient,
    name: &str,
) -> Result<Vec<String>> {
    use crate::engine::packages;

    let paths = VolumePaths::from_env()?;
    let mount_point = paths.mount_point(name);
    let declared = match packages::read_declared(&mount_point)? {
        Some(list) if !list.packages.is_empty() => list.packages,
        _ => return Ok(Vec::new()),
    };

    let names: Vec<&str> = declared.iter().map(String::as_str).collect();
    let mut argv = vec!["dpkg-query", "-W", "-f", "${Package} ${Status}\n"];
    argv.extend(names.iter());
    // dpkg-query exits non-zero when ANY name is unknown, which for a fresh
    // import is all of them — so the exit status is expected noise and the
    // OUTPUT is the evidence.
    let (_, output) = exec_capture(client, name, &argv).await?;

    let present: std::collections::BTreeSet<&str> = output
        .lines()
        .filter(|l| l.ends_with("install ok installed"))
        .filter_map(|l| l.split_whitespace().next())
        .collect();
    Ok(declared
        .iter()
        .filter(|p| !present.contains(p.as_str()))
        .cloned()
        .collect())
}

/// Detect the owner's installed packages and write `.nemr-state/packages.json`.
///
/// Returns Ok(Some(n)) with the count when the list was (re)written, Ok(None)
/// when there is nothing to declare AND no stale file to correct. The file is
/// byte-stable for identical content (see `packages::to_json`), so re-running
/// export on an unchanged project rewrites identical bytes and bundle
/// determinism holds.
async fn refresh_declared_packages(
    client: &ContainerdClient,
    container_id: &str,
    mount_point: &std::path::Path,
) -> Result<Option<usize>> {
    use crate::engine::packages;

    let (upper, lowers) = client.snapshot_overlay_dirs(container_id).await?;
    let pid = netns_rootlesskit_pid()?;
    let files = tokio::task::spawn_blocking(move || {
        packages::read_snapshot_package_files(&pid, &upper, &lowers)
    })
    .await
    .context("the snapshot read could not be scheduled")??;

    let declared = packages::declared(
        &files.container_status,
        &files.base_status,
        &files.container_extended_states,
    );

    let target = mount_point.join(packages::DeclaredPackages::VOLUME_PATH);
    if declared.is_empty() {
        // No declaration — but a STALE file from before packages were removed
        // must not travel and resurrect them on the destination. Remove rather
        // than leave, and absence-with-no-file is the common case: no write.
        if target.exists() {
            std::fs::remove_file(&target)
                .with_context(|| format!("removing the stale {}", target.display()))?;
            return Ok(Some(0));
        }
        return Ok(None);
    }

    let count = declared.len();
    let json = packages::to_json(&packages::DeclaredPackages::new(declared))?;
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(&target, json).with_context(|| format!("writing {}", target.display()))?;
    Ok(Some(count))
}

/// rootlesskit's child pid, for entering its mount namespace. Re-exported from
/// the netns module's locator so there is exactly one way to find it.
fn netns_rootlesskit_pid() -> Result<String> {
    crate::engine::netns::rootlesskit_child_pid_for_reads()
}

/// The container record for a project by its container id.
///
/// Used where the caller already holds a record and needs to re-read it under a
/// lock, so it must not go back through the name.
async fn find_container_by_id(
    client: &ContainerdClient,
    id: &str,
) -> Result<crate::containerd::containers::ContainerSummary> {
    client
        .list_containers()
        .await?
        .into_iter()
        .find(|c| c.id == id)
        .ok_or_else(|| {
            anyhow::anyhow!("container {id:?} disappeared while allocating its session network")
        })
}

/// The forwards a project declares, from its container label.
pub fn ports_from_labels(
    labels: &std::collections::HashMap<String, String>,
) -> Vec<ports::PortForward> {
    labels
        .get(LABEL_PORTS)
        .map(|raw| ports::decode_label(raw))
        .unwrap_or_default()
}

/// Read one project's declared forwards.
pub async fn list_ports(
    client: &ContainerdClient,
    name: &str,
) -> crate::error::Result<Vec<ports::PortForward>> {
    let container = find_container(client, name).await?;
    Ok(ports_from_labels(&container.labels))
}

/// Which project (if any) declares a forward on this host port.
///
/// The engine owns this mapping — rootlesskit knows only that some forward
/// holds the port, so naming the culprit is our job, and it is the difference
/// between a useful collision message and a shrug.
async fn project_holding_host_port(
    client: &ContainerdClient,
    host_port: u16,
    excluding: &str,
) -> crate::error::Result<Option<String>> {
    for c in client
        .list_containers()
        .await
        .map_err(crate::error::Error::Internal)?
    {
        let Some(project) = c.labels.get(LABEL_PROJECT) else {
            continue;
        };
        if project == excluding {
            continue;
        }
        if ports_from_labels(&c.labels)
            .iter()
            .any(|p| p.host_port == host_port)
        {
            return Ok(Some(project.clone()));
        }
    }
    Ok(None)
}

/// Declare a forward for a project, and apply it now if the project is running.
///
/// The declaration is what persists; the live forward is derived. A stopped
/// project records the port without holding it, so it never blocks another
/// project with a port it is not using.
pub async fn add_port(
    client: &ContainerdClient,
    name: &str,
    port: ports::PortForward,
) -> crate::error::Result<ports::PortForward> {
    use crate::error::Error;

    let container = find_container(client, name).await?;
    let container_id = config::container_id(name);
    let mut declared = ports_from_labels(&container.labels);

    if let Some(existing) = declared.iter().find(|p| p.host_port == port.host_port) {
        return Err(Error::PortRefused {
            detail: format!(
                "{name} already forwards host port {} (to container port {}). \
                 Remove it first: nemr port rm {name} {}",
                existing.host_port, existing.container_port, existing.host_port
            ),
        });
    }
    if let Some(other) = project_holding_host_port(client, port.host_port, name).await? {
        return Err(Error::PortRefused {
            detail: format!(
                "host port {} is already declared by project {other:?}. \
                 Choose another host port (e.g. nemr port add {name} {}:{}), \
                 or free it: nemr port rm {other} {}",
                port.host_port,
                ports::suggest_alternative(port.host_port),
                port.container_port,
                port.host_port
            ),
        });
    }

    // Only bind now if the project is running; a stopped project's declaration
    // is applied at start.
    let running = client
        .task_state(&container_id)
        .await
        .map_err(Error::Internal)?
        .is_running();
    if running {
        // NET-02: forward to the session's own address, not into rootlesskit's
        // namespace — the session no longer lives there.
        let live_target = match allocation_from_labels(&container.labels) {
            Some(alloc) => port.clone().via_session(&alloc.session_ip()),
            None => port.clone(),
        };
        apply_one(&live_target).map_err(|detail| Error::PortRefused { detail })?;
    } else if let Err(detail) = ports::host_port_is_free(&port.host_ip, port.host_port) {
        // The project is stopped, so nothing is bound yet — but accepting a
        // declaration that cannot work would defer the bad news to `start`,
        // where it is a warning the user may never read. Say it now.
        //
        // Only a genuine conflict gets the "something else on this machine"
        // framing: appending it to a permission failure (a port below 1024,
        // PRIV-01) described the wrong problem and sent the user hunting for a
        // process that does not exist (F-98).
        let detail = match detail.strip_prefix("IN_USE:") {
            Some(conflict) => format!(
                "{conflict} by something else on this machine (not a nemr project).\n\
                 Choose another host port — e.g. nemr port add {name} {}:{} — \
                 or stop whatever holds it.",
                ports::suggest_alternative(port.host_port),
                port.container_port
            ),
            None => detail,
        };
        return Err(Error::PortRefused { detail });
    }

    declared.push(port.clone());
    let mut labels = container.labels.clone();
    labels.insert(LABEL_PORTS.to_string(), ports::encode_label(&declared));
    client
        .update_container_labels(&container_id, labels)
        .await
        .map_err(Error::Internal)?;
    Ok(port)
}

/// Bind one forward now, translating rootlesskit's two refusals into messages
/// with different remedies.
fn apply_one(port: &ports::PortForward) -> std::result::Result<(), String> {
    match ports::add_live(port) {
        Ok(_) => Ok(()),
        Err(ports::AddFailure::HeldByTheHost { detail }) => Err(format!(
            "host port {} is in use by something else on this machine \
             (not a nemr project). Choose another host port, or stop whatever \
             holds it.\n  rootlesskit said: {detail}",
            port.host_port
        )),
        Err(ports::AddFailure::HeldByAForward { detail }) => Err(format!(
            "host port {} is already forwarded inside this engine.\n  \
             `nemr port ls <project>` shows which project declares it; \
             `nemr reconcile` reports (but never removes) forwards no project \
             claims.\n  rootlesskit said: {detail}",
            port.host_port
        )),
        Err(other) => Err(format!("could not forward port: {other}")),
    }
}

/// Stop declaring a forward, and drop it now if it is live.
pub async fn remove_port(
    client: &ContainerdClient,
    name: &str,
    host_port: u16,
) -> crate::error::Result<ports::PortForward> {
    use crate::error::Error;

    let container = find_container(client, name).await?;
    let container_id = config::container_id(name);
    let mut declared = ports_from_labels(&container.labels);

    let idx = declared
        .iter()
        .position(|p| p.host_port == host_port)
        .ok_or_else(|| Error::PortRefused {
            detail: format!(
                "{name} does not forward host port {host_port}. \
                 See what it does forward: nemr port ls {name}"
            ),
        })?;
    let removed = declared.remove(idx);

    // Drop the live forward if there is one. Best-effort by design: the
    // declaration is authoritative, so failing to unbind must not leave the
    // label claiming a port the project no longer wants.
    if let Ok(live) = ports::list_live() {
        for f in live.iter().filter(|f| f.host_port == host_port) {
            let _ = ports::remove_live(f.id);
        }
    }

    let mut labels = container.labels.clone();
    if declared.is_empty() {
        labels.remove(LABEL_PORTS);
    } else {
        labels.insert(LABEL_PORTS.to_string(), ports::encode_label(&declared));
    }
    client
        .update_container_labels(&container_id, labels)
        .await
        .map_err(Error::Internal)?;
    Ok(removed)
}

/// Re-apply a project's declared forwards. Called on `start`.
///
/// Forwards do not survive a rootlesskit restart, and are torn down on stop, so
/// the declaration is re-applied rather than assumed live. A port that cannot
/// be bound is reported and skipped: one unavailable port must not stop a
/// session from starting.
pub async fn apply_declared_ports(client: &ContainerdClient, name: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let Ok(container) = find_container(client, name).await else {
        return problems;
    };
    let declared = ports_from_labels(&container.labels);
    if declared.is_empty() {
        return problems;
    }
    let live = ports::list_live().unwrap_or_default();
    let alloc = allocation_from_labels(&container.labels);
    for port in declared {
        if live.iter().any(|f| f.host_port == port.host_port) {
            continue; // already bound; re-adding would collide with ourselves
        }
        let target = match &alloc {
            Some(a) => port.clone().via_session(&a.session_ip()),
            None => port.clone(),
        };
        if let Err(detail) = apply_one(&target) {
            problems.push(detail);
        }
    }
    problems
}

/// Drop a project's live forwards, leaving the declaration intact. Called on
/// `stop`, so a stopped project does not hold a host port it is not serving.
pub async fn withdraw_declared_ports(client: &ContainerdClient, name: &str) {
    let Ok(container) = find_container(client, name).await else {
        return;
    };
    let declared = ports_from_labels(&container.labels);
    if declared.is_empty() {
        return;
    }
    let Ok(live) = ports::list_live() else {
        return;
    };
    for port in &declared {
        for f in live.iter().filter(|f| f.host_port == port.host_port) {
            let _ = ports::remove_live(f.id);
        }
    }
}
