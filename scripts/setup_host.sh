#!/usr/bin/env bash
#
# Provision a development host for Nemr, from a clean Ubuntu to a host that
# passes acceptance.
#
# WHAT THIS IS. A development-host provisioner, and the executable spec for the
# product installer that has to replace it. Under D-01 and E-09 a user installs
# a package, a daemon starts, and they log in. Nothing here should survive into
# that experience — but everything here has to happen somewhere, so it is
# written down as code that runs rather than prose that gets transcribed by
# hand. Provisioning the second host by following the docs took about two hours;
# that is the finding this script exists to answer.
#
#   ./scripts/setup_host.sh
#
# Idempotent and re-runnable: it is expected to be run repeatedly against
# half-configured hosts, including immediately after the reboot it asks for.
#
# It refuses before it changes anything if the host cannot support the stack,
# and it finishes by running the acceptance suite — setup is not done because
# commands exited zero, it is done when the host passes verification.
#
# WHAT IS TESTED, AND WHAT IS NOT — read this before trusting a green run.
#
# TESTED: the preflight checks, each refusal path (kernel too old, cgroup v1,
# apparmor_restrict_unprivileged_userns=1, missing subuid/subgid warning rather
# than blocking), and the reboot gate including its distinct exit code 3. Those
# were exercised by injecting the failing condition and checking both the
# message and the exit status.
#
# NOT TESTED: everything from "Packages" onward. Those steps need root, and the
# only host available to develop on was already provisioned — so the privileged
# half of this script has never been executed end to end on a clean machine.
#
# That matters most for the steps that are hardest to get right on a host that
# does not already work: the package set, disabling the system containerd, the
# helper install, and the first base-image build. If this script fails for you
# somewhere below the reboot gate, that is the untested half, and
# PREREQUISITES.md is the reference for what each step is trying to achieve.
#
# CI provisions a clean Ubuntu runner via scripts/ci_provision_host.sh, which
# covers the same ground for the steps they share (packages, containerd
# disable, delegation, units, BuildKit, base image) — so those are exercised on
# a clean machine every run, just not through THIS script. The parts CI does
# not cover at all are the reboot resumption and the helper install.
#
# No failure is suppressed. `2>/dev/null` appears only on probes whose failure
# is the answer being measured, never on an action whose failure matters.

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
REPO="$PWD"

BLUE=$'\033[34m'; RED=$'\033[31m'; GREEN=$'\033[32m'; YELLOW=$'\033[33m'; RESET=$'\033[0m'
step() { printf '\n%s==> %s%s\n' "$BLUE" "$1" "$RESET"; }
ok()   { printf '    %sok%s   %s\n' "$GREEN" "$RESET" "$1"; }
warn() { printf '    %swarn%s %s\n' "$YELLOW" "$RESET" "$1"; }

# WSL2 is a second supported host (E-10): a Windows user runs this inside WSL2.
# A few steps below are WSL2-specific and inert elsewhere; this is how they know.
is_wsl2() { grep -qi microsoft /proc/sys/kernel/osrelease 2>/dev/null; }

PREFLIGHT_FAILURES=()
fail_check() {
    # name / what was found / how to fix — all three, every time. A preflight
    # that says only "unsupported" makes the reader guess at the remedy.
    PREFLIGHT_FAILURES+=("$1"$'\n'"        found: $2"$'\n'"        fix:   $3")
}

USER_NAME="$(id -un)"
USER_ID="$(id -u)"
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/${USER_ID}}"
export DBUS_SESSION_BUS_ADDRESS="${DBUS_SESSION_BUS_ADDRESS:-unix:path=${XDG_RUNTIME_DIR}/bus}"

# ---------------------------------------------------------------------------
# 1. Preflight — refuse before touching anything
# ---------------------------------------------------------------------------
step "Preflight (refusing early beats failing downstream)"

if [[ "$USER_ID" -eq 0 ]]; then
    echo "${RED}Run this as your normal user, not root.${RESET}" >&2
    echo "The whole point is a rootless stack (PRIV-01); the script asks for sudo where it needs it." >&2
    exit 2
fi

kernel="$(uname -r)"
kmajor="${kernel%%.*}"; krest="${kernel#*.}"; kminor="${krest%%.*}"
if (( kmajor > 5 || (kmajor == 5 && kminor >= 8) )); then
    ok "kernel $kernel (>= 5.8)"
else
    fail_check "kernel >= 5.8 (rootless cgroup v2 delegation)" "$kernel" \
        "upgrade the kernel; 5.8 is where the required cgroup v2 behaviour landed"
fi

if [[ -e /sys/fs/cgroup/cgroup.controllers ]]; then
    ok "cgroup v2 unified hierarchy"
else
    fail_check "cgroup v2 unified hierarchy" "/sys/fs/cgroup/cgroup.controllers absent (cgroup v1?)" \
        "boot with systemd.unified_cgroup_hierarchy=1, or use a distribution that defaults to cgroup v2"
fi

# Ubuntu 23.10+ ships this as 1, which blocks unprivileged user namespaces and
# therefore PRIV-01 outright. It has to be a clear refusal here: downstream it
# surfaces as an opaque rootlesskit failure.
userns_knob=/proc/sys/kernel/apparmor_restrict_unprivileged_userns
if [[ -e "$userns_knob" ]]; then
    value="$(cat "$userns_knob")"
    if [[ "$value" == "0" ]]; then
        ok "apparmor_restrict_unprivileged_userns = 0"
    else
        fail_check "kernel.apparmor_restrict_unprivileged_userns must be 0" "$value" \
            "sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0
               persist it:  echo 'kernel.apparmor_restrict_unprivileged_userns=0' | sudo tee /etc/sysctl.d/99-nemr-userns.conf
               Ubuntu 23.10+ ships this as 1; rootless containerd cannot start without it."
    fi
else
    ok "no apparmor userns restriction on this kernel"
fi

# The functional check, not just the knob: the knob is the usual cause but not
# the only one, and this is what actually has to work.
if unshare -rmn true 2>/dev/null; then
    ok "unprivileged user/mount/network namespaces work"
else
    detail="$(unshare -rmn true 2>&1 || true)"
    fail_check "unprivileged namespaces must be usable" "${detail:-unshare -rmn failed}" \
        "usually the apparmor knob above; otherwise check that user namespaces are enabled in the kernel"
fi

for db in subuid subgid; do
    if grep -q "^${USER_NAME}:" "/etc/$db" 2>/dev/null; then
        ok "/etc/$db has a range for $USER_NAME"
    else
        fail_check "/etc/$db must contain a range for $USER_NAME" "no entry" \
            "sudo usermod --add-${db/sub/sub}s 100000-165535 $USER_NAME   (this script does it for you if preflight passes)"
    fi
done

if [[ -d /run/systemd/system ]]; then
    ok "systemd is the init system"
else
    fail_check "systemd required (user units run the rootless daemons)" "/run/systemd/system absent" \
        "this stack is systemd-only; see E-10 for the non-Linux/non-systemd position"
fi

# subuid/subgid are fixable by this script, so they are reported but do not
# block. Everything else is the host's, not ours.
BLOCKING=()
for failure in "${PREFLIGHT_FAILURES[@]:-}"; do
    [[ -z "$failure" ]] && continue
    case "$failure" in
        /etc/subuid*|/etc/subgid*) warn "will fix: ${failure%%$'\n'*}" ;;
        *) BLOCKING+=("$failure") ;;
    esac
done

if (( ${#BLOCKING[@]} > 0 )); then
    printf '\n%sPreflight failed — nothing has been changed.%s\n\n' "$RED" "$RESET" >&2
    for failure in "${BLOCKING[@]}"; do
        printf '  - %s\n\n' "$failure" >&2
    done
    exit 1
fi
ok "preflight passed"

# ---------------------------------------------------------------------------
# 2. Packages
# ---------------------------------------------------------------------------
step "Packages (Ubuntu archive only — no Docker repository, NFR-01)"
sudo apt-get update
sudo apt-get install -y \
    containerd runc \
    protobuf-compiler build-essential curl \
    uidmap rootlesskit slirp4netns \
    e2fsprogs

# ---------------------------------------------------------------------------
# 3. Host configuration
# ---------------------------------------------------------------------------
step "Disable the system-wide root containerd (PRIV-01)"
# Leaving it running means a socket-resolution mistake could silently reach the
# root daemon — the "green but wrong" class this project keeps hitting.
if systemctl is-enabled --quiet containerd.service 2>/dev/null ||
   systemctl is-active --quiet containerd.service 2>/dev/null; then
    sudo systemctl disable --now containerd.service
    ok "disabled containerd.service"
else
    ok "containerd.service already disabled"
fi

step "Mount propagation (F-128 — WSL2 only; a no-op on a standard host)"
# WSL2's /init leaves / a PRIVATE mount where a standard systemd host makes it
# rshared. Without shared propagation, the privileged helper's volume mount
# never reaches rootlesskit's namespace and every project fails to start with an
# opaque "no such file or directory" (F-128). Two halves: persist it for every
# future boot (the unit, ordered before the rootless stack — the ordering the
# WSL2 spike proved necessary), and make it live now so this run's own
# verification passes without waiting for a reboot.
if is_wsl2; then
    PROP_UNIT_SRC="deploy/systemd/nemr-mount-propagation.service"
    PROP_UNIT_DEST="/etc/systemd/system/nemr-mount-propagation.service"
    if [[ -e "$PROP_UNIT_DEST" ]] && cmp -s "$PROP_UNIT_SRC" "$PROP_UNIT_DEST"; then
        ok "nemr-mount-propagation.service already installed"
    else
        sudo install -D -m 0644 "$PROP_UNIT_SRC" "$PROP_UNIT_DEST"
        sudo systemctl daemon-reload
        ok "installed $PROP_UNIT_DEST"
    fi
    sudo systemctl enable nemr-mount-propagation.service
    sudo mount --make-rshared /
    ok "/ is now shared (live), and set to be made shared on every boot"
else
    ok "not WSL2 — a standard systemd host makes / rshared at boot; nothing to do"
fi

step "subuid/subgid ranges for $USER_NAME"
grep -q "^${USER_NAME}:" /etc/subuid || sudo usermod --add-subuids 100000-165535 "$USER_NAME"
grep -q "^${USER_NAME}:" /etc/subgid || sudo usermod --add-subgids 100000-165535 "$USER_NAME"
ok "present"

step "cgroup v2 controller delegation"
DELEGATE_DEST=/etc/systemd/system/user@.service.d/delegate.conf
sudo mkdir -p "$(dirname "$DELEGATE_DEST")"
if [[ -e "$DELEGATE_DEST" ]] && cmp -s deploy/systemd/delegate.conf "$DELEGATE_DEST"; then
    ok "delegate.conf already installed"
else
    sudo install -D -m 0644 deploy/systemd/delegate.conf "$DELEGATE_DEST"
    sudo systemctl daemon-reload
    ok "installed $DELEGATE_DEST"
fi

step "Lingering (so the user manager runs without an interactive login)"
if loginctl show-user "$USER_NAME" --property=Linger 2>/dev/null | grep -q 'Linger=yes'; then
    ok "already enabled"
else
    sudo loginctl enable-linger "$USER_NAME"
    ok "enabled"
fi

# ---------------------------------------------------------------------------
# 4. The reboot gate — surfaced, not hidden
# ---------------------------------------------------------------------------
# Delegation takes effect when user@.service restarts. Restarting it kills the
# session doing the restarting, so in practice this is a reboot. Rather than
# pretend otherwise, stop here and resume cleanly on the next run.
step "Checking whether cgroup delegation is live"
USER_SLICE="/sys/fs/cgroup/user.slice/user-${USER_ID}.slice/user@${USER_ID}.service/cgroup.controllers"
delegated=""
[[ -r "$USER_SLICE" ]] && delegated="$(cat "$USER_SLICE")"

if [[ " $delegated " == *" cpu "* ]]; then
    ok "delegated controllers: $delegated"
else
    cat >&2 <<EOF

${YELLOW}Reboot required.${RESET}

  cgroup delegation is configured but not yet in effect.
      expected: cpu among the delegated controllers
      found:    ${delegated:-<user slice not readable>}

  Delegation applies when user@${USER_ID}.service restarts, and restarting it
  kills the session that asks — so this is a reboot in practice.

      sudo reboot

  Then run this script again. It is idempotent; it will skip everything above
  and continue from here.

EOF
    exit 3
fi

# ---------------------------------------------------------------------------
# 5. User units
# ---------------------------------------------------------------------------
step "Install the rootless user units"
mkdir -p ~/.config/systemd/user
units_changed=0
for unit in deploy/systemd/user/*.service; do
    dest="$HOME/.config/systemd/user/$(basename "$unit")"
    if [[ -e "$dest" ]] && cmp -s "$unit" "$dest"; then
        ok "$(basename "$unit") up to date"
    else
        install -D -m 0644 "$unit" "$dest"
        units_changed=1
        ok "installed $(basename "$unit")"
    fi
done
(( units_changed )) && systemctl --user daemon-reload

# ---------------------------------------------------------------------------
# 6. BuildKit — the one binary fetched from outside the distribution
# ---------------------------------------------------------------------------
step "BuildKit (daemonless OCI builder, SPEC §3.1)"
BUILDKIT_VERSION="${BUILDKIT_VERSION:-0.32.2}"
BUILDKIT_SHA256="${BUILDKIT_SHA256:-2975d0f651ad96ba8b80b9992ae1f9a964f4408569af5b6dc36544165c3926af}"
mkdir -p "$HOME/.local/bin"
export PATH="$HOME/.local/bin:$PATH"
if command -v buildctl >/dev/null && command -v buildkitd >/dev/null; then
    ok "buildctl $(buildctl --version | awk '{print $3}') already installed"
else
    tarball="buildkit-v${BUILDKIT_VERSION}.linux-amd64.tar.gz"
    url="https://github.com/moby/buildkit/releases/download/v${BUILDKIT_VERSION}/${tarball}"
    echo "    fetching $url"
    curl -fsSL --proto '=https' --tlsv1.2 -o "/tmp/${tarball}" "$url"
    echo "${BUILDKIT_SHA256}  /tmp/${tarball}" | sha256sum -c -
    tar -xzf "/tmp/${tarball}" -C /tmp
    install -D -m 0755 /tmp/bin/buildctl "$HOME/.local/bin/buildctl"
    install -D -m 0755 /tmp/bin/buildkitd "$HOME/.local/bin/buildkitd"
    rm -rf "/tmp/${tarball}" /tmp/bin
    ok "installed buildctl and buildkitd to ~/.local/bin"
fi

case ":$PATH:" in
    *":$HOME/.local/bin:"*) ;;
    *) warn "$HOME/.local/bin is not on your PATH; add it to your shell profile" ;;
esac

# ---------------------------------------------------------------------------
# 7. Start the daemons
# ---------------------------------------------------------------------------
step "Start rootless containerd and buildkitd"
systemctl --user enable --now containerd-rootless.service
systemctl --user enable --now buildkitd-rootless.service

sock="${XDG_RUNTIME_DIR}/containerd/containerd.sock"
for _ in $(seq 1 30); do [[ -S "$sock" ]] && break; sleep 1; done
if [[ ! -S "$sock" ]]; then
    echo "${RED}rootless containerd did not come up.${RESET}" >&2
    systemctl --user status containerd-rootless.service --no-pager >&2 || true
    journalctl --user -u containerd-rootless.service -n 50 --no-pager >&2 || true
    exit 1
fi
export CONTAINERD_ADDRESS="$sock"
ok "containerd answering at $sock"

bksock="${XDG_RUNTIME_DIR}/buildkit/buildkitd.sock"
for _ in $(seq 1 30); do [[ -S "$bksock" ]] && break; sleep 1; done
if [[ ! -S "$bksock" ]]; then
    echo "${RED}rootless buildkitd did not come up.${RESET}" >&2
    systemctl --user status buildkitd-rootless.service --no-pager >&2 || true
    journalctl --user -u buildkitd-rootless.service -n 50 --no-pager >&2 || true
    exit 1
fi
ok "buildkitd answering at $bksock"

if is_wsl2; then
    step "WSL2: restart the rootless stack so it snapshots the shared mount (F-128)"
    # `enable --now` does not restart an already-running daemon, and rootlesskit's
    # mount namespace is a snapshot from when it started — so a containerd that
    # came up at boot (lingering) BEFORE / was made shared cannot see the shared
    # propagation until it restarts. The WSL2 spike proved exactly this: making /
    # rshared did nothing until the rootless stack was restarted.
    systemctl --user restart containerd-rootless.service
    for _ in $(seq 1 30); do [[ -S "$sock" ]] && break; sleep 1; done
    if [[ ! -S "$sock" ]]; then
        echo "${RED}containerd did not come back after the restart.${RESET}" >&2
        journalctl --user -u containerd-rootless.service -n 50 --no-pager >&2 || true
        exit 1
    fi
    if systemctl --user restart nemrd.service 2>/dev/null; then
        ok "rootless stack restarted; the shared mount is live in its namespace"
    else
        warn "containerd restarted; nemrd will restart on demand"
    fi
fi

# ---------------------------------------------------------------------------
# 7b. Shell environment — so manual commands find the rootless socket
# ---------------------------------------------------------------------------
# Divergence 6 of the WSL2 spike (class: onboarding-path-untested, not WSL2):
# `nemr` resolves the socket itself, but a bare `ctr` — and the base-image
# remedies below — need CONTAINERD_ADDRESS in the interactive shell, which setup
# only ever set inside its own process. Persist it, idempotently, in a
# clearly-marked managed block. ~/.local/bin goes on PATH here too, which is the
# warning section 6 could only print.
step "Shell environment (CONTAINERD_ADDRESS, ~/.local/bin) for interactive use"
PROFILE="$HOME/.bashrc"
PROFILE_MARKER="# >>> nemr environment (managed by setup_host.sh) >>>"
if [[ -f "$PROFILE" ]] && grep -qF "$PROFILE_MARKER" "$PROFILE"; then
    ok "$PROFILE already has the nemr environment block"
else
    {
        printf '\n%s\n' "$PROFILE_MARKER"
        echo 'export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"'
        echo 'export CONTAINERD_ADDRESS="${CONTAINERD_ADDRESS:-$XDG_RUNTIME_DIR/containerd/containerd.sock}"'
        echo 'case ":$PATH:" in *":$HOME/.local/bin:"*) ;; *) export PATH="$HOME/.local/bin:$PATH" ;; esac'
        echo '# <<< nemr environment <<<'
    } >> "$PROFILE"
    ok "added CONTAINERD_ADDRESS and ~/.local/bin to $PROFILE (open a new shell, or source it)"
fi

# ---------------------------------------------------------------------------
# 8. Nemr's own artifacts
# ---------------------------------------------------------------------------
step "Install the engine"
./scripts/install_engine.sh

step "Install the privileged helper (needs root — PRIV-02/03)"
(cd deploy/nemr-volume && cargo build --release)
sudo ./scripts/setup_test_host.sh

step "Obtain the base image (pull the published bytes; build only as fallback — F-126)"
./scripts/fetch_base_image.sh

# ---------------------------------------------------------------------------
# 8b. Claude Code CLI — a prerequisite we detect, never install (D-13)
# ---------------------------------------------------------------------------
# nemr runs Claude Code inside each project; the CLI and its Node runtime are a
# stated prerequisite, not something this script installs. That mirrors how the
# GPU/CUDA host is treated (E-10) and follows NFR-01's spirit one layer out: nemr
# does not reach outside the distribution archive, and adding a third-party apt
# repo (NodeSource) to a user's system, or running a global `npm` install for
# them, is exactly that. The WSL2 spike hit this as divergence 3 (Node and Claude
# Code absent, `npm -g` EACCES). Detect and instruct; never install. Non-fatal:
# the engine and its suite provision fully without it (fixtures + placeholder
# credential); it is `nemr create` that needs a real Claude Code + credential.
step "Claude Code CLI (a prerequisite — detected, never installed — D-13)"
if command -v claude >/dev/null 2>&1; then
    ok "claude present ($(command -v claude))"
elif command -v node >/dev/null 2>&1; then
    warn "node is installed but 'claude' is not on PATH. Install the CLI from its
        official instructions (npm: '@anthropic-ai/claude-code', or the native
        installer) — nemr does not install it for you (D-13). See PREREQUISITES.md."
else
    warn "Claude Code and its Node runtime are not installed — a prerequisite (D-13).
        Install Node from nodejs.org or your platform's own packaging (NOT a
        third-party apt repo added by nemr), then Claude Code per its official
        instructions. nemr states this prerequisite; it does not reach outside the
        archive to satisfy it (NFR-01). See PREREQUISITES.md."
fi

# ---------------------------------------------------------------------------
# 9. Credential
# ---------------------------------------------------------------------------
step "Claude Code credential (AUTH-01/02/03)"
if [[ -e "$HOME/.claude/.credentials.json" ]]; then
    ok "credential present at ~/.claude/.credentials.json"
else
    cat <<EOF

    No credential at ~/.claude/.credentials.json.

    \`nemr create\` fails without one, deliberately (AUTH-03): there is no point
    provisioning storage for a container that cannot authenticate.

        claude   # log in, then re-run this script

EOF
fi

# ---------------------------------------------------------------------------
# 9b. Developer workflow: the fmt pre-push guard
# ---------------------------------------------------------------------------
# Structural fix for a lapse that recurred in WP-J (pushing code that was
# clippy-checked but not fmt-checked). Repo-local, no root.
step "Install git hooks (fmt pre-push guard)"
chmod +x scripts/git-hooks/* 2>/dev/null || true
git config core.hooksPath scripts/git-hooks
ok "core.hooksPath = scripts/git-hooks"

# ---------------------------------------------------------------------------
# 9c. Sync-server test database (WP-J) — the Postgres nemr-sync tests need
# ---------------------------------------------------------------------------
# Orthogonal to the engine, so this is non-fatal: a failure here (e.g. no podman
# yet) must not fail engine provisioning. Skip with NEMR_SKIP_SYNC_DB=1.
step "Provision the sync-server test database (Postgres, WP-J)"
if [[ "${NEMR_SKIP_SYNC_DB:-0}" == "1" ]]; then
    ok "skipped (NEMR_SKIP_SYNC_DB=1)"
elif ./scripts/setup_sync_test_db.sh; then
    ok "sync-server test Postgres is up (see the printed DATABASE_URL)"
else
    warn "sync-server test DB not provisioned; run ./scripts/setup_sync_test_db.sh later"
fi

# ---------------------------------------------------------------------------
# 9d. Sync client (WP-K) — the commercial half's CLI
# ---------------------------------------------------------------------------
# Non-fatal like the test DB: the open engine must provision fully without it.
step "Install the sync client (nemr login / push / pull / sessions)"
if ./scripts/install_sync_client.sh; then
    ok "sync client installed"
else
    warn "sync client not installed; run ./scripts/install_sync_client.sh later"
fi

# ---------------------------------------------------------------------------
# 10. Acceptance — setup is done when the host passes, not when commands exit 0
# ---------------------------------------------------------------------------
step "Verification"
echo "    Setup is not finished because commands exited zero. Running acceptance."
./scripts/verify_wp_a.sh

printf '\n%sHost provisioned and verified.%s\n' "$GREEN" "$RESET"
