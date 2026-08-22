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
# No failure is suppressed. `2>/dev/null` appears only on probes whose failure
# is the answer being measured, never on an action whose failure matters.

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
REPO="$PWD"

BLUE=$'\033[34m'; RED=$'\033[31m'; GREEN=$'\033[32m'; YELLOW=$'\033[33m'; RESET=$'\033[0m'
step() { printf '\n%s==> %s%s\n' "$BLUE" "$1" "$RESET"; }
ok()   { printf '    %sok%s   %s\n' "$GREEN" "$RESET" "$1"; }
warn() { printf '    %swarn%s %s\n' "$YELLOW" "$RESET" "$1"; }

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

# ---------------------------------------------------------------------------
# 8. Nemr's own artifacts
# ---------------------------------------------------------------------------
step "Install the engine"
./scripts/install_engine.sh

step "Install the privileged helper (needs root — PRIV-02/03)"
(cd deploy/nemr-volume && cargo build --release)
sudo ./scripts/setup_test_host.sh

step "Build and import the base image"
./scripts/build_base_image.sh

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
# 10. Acceptance — setup is done when the host passes, not when commands exit 0
# ---------------------------------------------------------------------------
step "Verification"
echo "    Setup is not finished because commands exited zero. Running acceptance."
./scripts/verify_wp_a.sh

printf '\n%sHost provisioned and verified.%s\n' "$GREEN" "$RESET"
