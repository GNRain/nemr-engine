#!/usr/bin/env bash
#
# nemr — one command to install the engine on this machine (D-14).
#
#   ./scripts/install.sh            # shows the plan, asks once, installs
#   ./scripts/install.sh --yes      # same, without the question
#   ./scripts/install.sh --quiet    # no animation; step lines only
#
# WHAT THIS IS. Everything a person needs on the machine they work on: the
# rootless container stack, the privileged volume helper and its one sudoers
# grant, the base image, the engine (`nemr`, `nemrd`) and the client CLI
# (`nemr ui`, `nemr push`, `nemr pull`). It replaces the dozen commands across
# three sessions that installing on the second VM took.
#
# WHAT IT IS NOT. It is not the developer provisioner — scripts/setup_host.sh
# keeps that job, with BuildKit, the test database and the git hooks. And it is
# not the server: someone running their own sync server uses
# scripts/install_server.sh. Neither is called from here (D-14).
#
# CONSENT. It shows the whole plan first — every file it writes, every command
# it runs under sudo, everything it downloads — and asks once. `--yes` skips the
# question; with no terminal and no `--yes` it refuses and names the flag rather
# than proceeding silently (F-15). Declining changes nothing, so answering "n"
# is the dry run.
#
# It is safe to run twice: every step is probed first, and a step already done
# says so instead of being redone.

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
REPO="$PWD"

. scripts/lib/cat.sh
. scripts/lib/region.sh
. scripts/lib/steps.sh
# shellcheck source=lib/proc.sh
. scripts/lib/proc.sh
. scripts/lib/base_image.sh

# ---------------------------------------------------------------------------
# Flags
# ---------------------------------------------------------------------------
YES=0
usage() {
    cat <<'USAGE'
nemr install — the engine, the helper, the base image and the CLI, on this machine.

  ./scripts/install.sh          show the plan, ask once, install
  ./scripts/install.sh --yes    accept the plan without the question
  ./scripts/install.sh --quiet  no animation; step lines only
  ./scripts/install.sh --help   this

It is safe to run twice: a step already done says so instead of being redone.
Running your own sync server instead? That is scripts/install_server.sh (D-14).
USAGE
}
while (($#)); do
    case "$1" in
        -y|--yes)   YES=1 ;;
        -q|--quiet) NEMR_CAT=0 ;;
        -h|--help)  usage; exit 0 ;;
        *) printf 'install.sh: unknown option %s\n\n' "$1" >&2; usage >&2; exit 2 ;;
    esac
    shift
done
export NEMR_CAT="${NEMR_CAT:-}"

USER_NAME="$(id -un)"
USER_ID="$(id -u)"
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/${USER_ID}}"
export DBUS_SESSION_BUS_ADDRESS="${DBUS_SESSION_BUS_ADDRESS:-unix:path=${XDG_RUNTIME_DIR}/bus}"
export CONTAINERD_ADDRESS="${CONTAINERD_ADDRESS:-${XDG_RUNTIME_DIR}/containerd/containerd.sock}"
export PATH="$HOME/.local/bin:$PATH"
is_wsl2() { grep -qi microsoft /proc/sys/kernel/osrelease 2>/dev/null; }
have() { command -v "$1" >/dev/null 2>&1; }

BASE_IMAGE="$(nemr_base_image)"
BASE_VERSION="${BASE_IMAGE##*:}"
BASE_DIGEST="$(cat "image/digests/${BASE_VERSION}" 2>/dev/null || true)"

DELEGATED_CONTROLLERS_PATH="/sys/fs/cgroup/user.slice/user-${USER_ID}.slice/user@${USER_ID}.service/cgroup.controllers"

PROFILE_MARKER="# >>> nemr environment (managed by scripts/install.sh) >>>"

# The set setup_host.sh provisioned both reference hosts with — not a new list.
# containerd/runc are the runtime; uidmap/rootlesskit/slirp4netns are what makes
# it rootless (PRIV-01); e2fsprogs formats the loopback volumes;
# build-essential/protobuf-compiler/curl are what building nemr here needs.
APT_PACKAGES=(containerd runc
              uidmap rootlesskit slirp4netns
              e2fsprogs
              build-essential protobuf-compiler curl)

# ---------------------------------------------------------------------------
# Preflight — refuse before the plan, naming what is missing and how to get it.
#
# Everything here is something this installer will NOT fix: a host that cannot
# run the stack, or a prerequisite that is deliberately not ours to install
# (D-13, NFR-01). A refusal here has changed nothing at all — which is the
# point: half an install is worse than none.
# ---------------------------------------------------------------------------
MISSING=()
refuse() { MISSING+=("$1"$'\n'"      found: $2"$'\n'"      fix:   $3"); }

preflight() {
    if [[ "$USER_ID" -eq 0 ]]; then
        printf 'Run this as your normal user, not root.\n' >&2
        printf 'The stack is rootless (PRIV-01); the script asks for sudo where it needs it.\n' >&2
        exit 2
    fi

    # Test seams (test-only, value-only — the pattern fetch_base_image.sh uses).
    # They let scripts/test_install.sh prove each refusal on a host that passes.
    local kernel="${NEMR_TEST_KERNEL:-$(uname -r)}"
    local cgroup_marker="${NEMR_TEST_CGROUP_MARKER:-/sys/fs/cgroup/cgroup.controllers}"
    local userns_knob="${NEMR_TEST_USERNS_KNOB:-/proc/sys/kernel/apparmor_restrict_unprivileged_userns}"

    local kmajor="${kernel%%.*}" krest="${kernel#*.}" kminor
    kminor="${krest%%.*}"
    if ! [[ "$kmajor" =~ ^[0-9]+$ && "$kminor" =~ ^[0-9]+$ ]] ||
       ! (( kmajor > 5 || (kmajor == 5 && kminor >= 8) )); then
        refuse "a kernel of 5.8 or newer (rootless cgroup v2 delegation)" "$kernel" \
            "upgrade the kernel — 5.8 is where the cgroup v2 behaviour this depends on landed"
    fi

    [[ -e "$cgroup_marker" ]] || refuse "the cgroup v2 unified hierarchy" \
        "$cgroup_marker is absent (a cgroup v1 host?)" \
        "boot with systemd.unified_cgroup_hierarchy=1, or use a distribution that defaults to cgroup v2"

    [[ -d /run/systemd/system ]] || refuse "systemd (the rootless daemons run as user units)" \
        "/run/systemd/system is absent" \
        "this stack is systemd-only; see E-10 for the position on other hosts"

    if [[ -e "$userns_knob" ]] && [[ "$(cat "$userns_knob")" != "0" ]]; then
        refuse "kernel.apparmor_restrict_unprivileged_userns = 0" "$(cat "$userns_knob")" \
            "sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0
             persist it: echo 'kernel.apparmor_restrict_unprivileged_userns=0' | sudo tee /etc/sysctl.d/99-nemr-userns.conf
             Ubuntu 23.10+ ships this as 1; rootless containerd cannot start with it set."
    elif [[ -z "${NEMR_TEST_USERNS_KNOB:-}" ]] && ! unshare -rmn true 2>/dev/null; then
        # The functional check, not only the knob: the knob is the usual cause,
        # not the only one, and this is what actually has to work.
        refuse "usable unprivileged user, mount and network namespaces" \
            "$(unshare -rmn true 2>&1 || true)" \
            "usually the apparmor knob above; otherwise check that user namespaces are enabled in the kernel"
    fi

    [[ "$(uname -m)" == "x86_64" ]] || refuse "x86_64 (the base image is linux/amd64 only)" \
        "$(uname -m)" "no fix here — the published base image has one platform"

    have apt-get || refuse "apt-get (this installer provisions Debian and Ubuntu)" \
        "no apt-get on PATH" \
        "on another distribution, install the packages in PREREQUISITES.md by hand, then run scripts/install_engine.sh"

    have sudo || refuse "sudo (the volume helper needs one root-owned install)" \
        "no sudo on PATH" "install sudo and give this account a grant, then run this again"

    have cargo || refuse "the Rust toolchain (nemr is built from source here)" \
        "no cargo on PATH" \
        "install rustup: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
             then: source \"\$HOME/.cargo/env\""

    # Disk. The base image is 332 MiB in the content store plus its unpacked
    # snapshot, and a release build tree is several GB. Below this floor the
    # build fails late, with a linker error that names nothing useful.
    local avail_kb
    avail_kb="${NEMR_TEST_AVAIL_KB:-$(df -Pk "$REPO" | awk 'NR==2 {print $4}')}"
    if (( avail_kb < 5 * 1024 * 1024 )); then
        refuse "at least 5 GiB free where this repository lives" \
            "$(( avail_kb / 1024 / 1024 )) GiB free at $REPO" \
            "free some space; the build tree and the base image need it"
    fi

    if (( ${#MISSING[@]} > 0 )); then
        printf '\n%snemr cannot be installed on this machine yet — nothing has been changed.%s\n\n' \
            "$_S_RED" "$_S_RESET" >&2
        local m
        for m in "${MISSING[@]}"; do printf '  needs: %s\n\n' "$m" >&2; done
        printf 'Fix these and run this again. PREREQUISITES.md explains why each one matters.\n' >&2
        exit 1
    fi
}

# ---------------------------------------------------------------------------
# The steps. Each has a probe (is it already done?) and an action.
# ---------------------------------------------------------------------------
STEP_IDS=()
declare -A STEP_LABEL=() STEP_STATE=() STEP_DETAIL=()

step_def() { STEP_IDS+=("$1"); STEP_LABEL["$1"]="$2"; }

step_def packages   "packages from the Ubuntu archive"
step_def containerd_off "the system-wide root containerd, disabled"
is_wsl2 && step_def propagation "shared mount propagation (WSL2)"
step_def subids     "subuid/subgid ranges for $USER_NAME"
step_def delegation "cgroup v2 controller delegation"
step_def linger     "lingering, so the user manager runs without a login"
step_def units      "the rootless containerd and nemrd user units"
step_def daemons    "rootless containerd, running"
step_def shellenv   "PATH and CONTAINERD_ADDRESS in ~/.bashrc"
step_def engine     "the engine, built and installed (nemr, nemrd)"
step_def helper     "the privileged volume helper and its sudoers grant"
step_def client     "the client CLI (nemr ui, push, pull)"
step_def image      "the base image $BASE_IMAGE"
step_def claude     "Claude Code, a prerequisite (detected, never installed)"
step_def verify     "the host passes the smoke test"

missing_packages() {
    local p out=()
    for p in "${APT_PACKAGES[@]}"; do
        dpkg-query -W -f='${Status}' "$p" 2>/dev/null | grep -q "^install ok installed$" || out+=("$p")
    done
    printf '%s\n' "${out[*]}"
}

probe() {
    local id="$1" state=done detail=""
    case "$id" in
    packages)
        local miss; miss="$(missing_packages)"
        if [[ -n "$miss" ]]; then state=todo; detail="will install: $miss"
        else detail="all ${#APT_PACKAGES[@]} present"; fi ;;
    containerd_off)
        if systemctl is-enabled --quiet containerd.service 2>/dev/null ||
           systemctl is-active --quiet containerd.service 2>/dev/null; then
            state=todo; detail="will run: sudo systemctl disable --now containerd.service"
        else detail="already disabled"; fi ;;
    subids)
        if grep -q "^${USER_NAME}:" /etc/subuid 2>/dev/null && grep -q "^${USER_NAME}:" /etc/subgid 2>/dev/null; then
            detail="present"
        else state=todo; detail="will run: sudo usermod --add-subuids/--add-subgids 100000-165535 $USER_NAME"; fi ;;
    delegation)
        # The functional state, not the bytes of the drop-in: a host already
        # delegating cpu is configured, however it was configured, and
        # overwriting a working drop-in to match this repo's comments would be
        # a change for nothing. The reboot gate below checks the same fact.
        if [[ " $(cat "$DELEGATED_CONTROLLERS_PATH" 2>/dev/null || true) " == *" cpu "* ]]; then
            detail="cpu is delegated to this user"
        else state=todo; detail="will write: /etc/systemd/system/user@.service.d/delegate.conf (root:root 0644), then a reboot is needed"; fi ;;
    linger)
        if loginctl show-user "$USER_NAME" --property=Linger 2>/dev/null | grep -q 'Linger=yes'; then
            detail="enabled"
        else state=todo; detail="will run: sudo loginctl enable-linger $USER_NAME"; fi ;;
    propagation)
        if [[ -e /etc/systemd/system/nemr-mount-propagation.service ]] &&
           cmp -s deploy/systemd/nemr-mount-propagation.service /etc/systemd/system/nemr-mount-propagation.service; then
            detail="installed"
        else state=todo; detail="will write: /etc/systemd/system/nemr-mount-propagation.service, and mount --make-rshared /"; fi ;;
    units)
        local u todo=0
        for u in containerd-rootless nemrd; do
            cmp -s "deploy/systemd/user/$u.service" "$HOME/.config/systemd/user/$u.service" || todo=1
        done
        if (( todo )); then state=todo; detail="will write: ~/.config/systemd/user/{containerd-rootless,nemrd}.service"
        else detail="up to date"; fi ;;
    daemons)
        if [[ -S "$XDG_RUNTIME_DIR/containerd/containerd.sock" ]]; then detail="answering at $XDG_RUNTIME_DIR/containerd/containerd.sock"
        else state=todo; detail="will run: systemctl --user enable --now containerd-rootless.service"; fi ;;
    shellenv)
        # Either marker: a host provisioned by setup_host.sh already has this
        # block under its name, and appending a second one is the opposite of
        # idempotent.
        if grep -qE '^# >>> nemr environment' "$HOME/.bashrc" 2>/dev/null; then
            detail="the nemr block is in ~/.bashrc"
        else state=todo; detail="will append one marked block to ~/.bashrc (PATH, CONTAINERD_ADDRESS)"; fi ;;
    engine)
        if [[ -x "$HOME/.local/bin/nemr" && -x "$HOME/.local/bin/nemrd" ]]; then
            state=rebuild; detail="installed; will rebuild from source and reinstall if it changed"
        else state=todo; detail="will write: ~/.local/bin/nemr, ~/.local/bin/nemrd (built here from source)"; fi ;;
    helper)
        if [[ -x /usr/local/libexec/nemr-volume && -e /etc/sudoers.d/nemr-volume ]]; then
            state=rebuild; detail="installed; will rebuild and reinstall if it changed"
        else state=todo
             detail="will write: /usr/local/libexec/nemr-volume (root:root 0755) and /etc/sudoers.d/nemr-volume (root:root 0440, checked by visudo -c first)"; fi ;;
    client)
        if [[ -x "$HOME/.local/bin/nemr-cloud" ]]; then
            state=rebuild; detail="installed; will rebuild and reinstall if it changed"
        else state=todo; detail="will write: ~/.local/bin/nemr-cloud and 8 symlinks (nemr-login … nemr-ui)"; fi ;;
    image)
        if ctr -n default images ls 2>/dev/null | grep -qF "$BASE_IMAGE"; then
            state=rebuild; detail="present; will check it is the recorded digest"
        else state=todo; detail="will pull: $BASE_IMAGE (digest ${BASE_DIGEST:0:19}…, ~332 MiB)"; fi ;;
    claude)
        if have claude; then detail="present at $(command -v claude)"
        else state=todo; detail="NOT INSTALLED — nemr detects it and never installs it (D-13)"; fi ;;
    verify)
        state=rebuild
        detail="runs scripts/e2e_smoke_test.sh: create, start, attach, stop, delete, host clean" ;;
    esac
    STEP_STATE["$id"]="$state"
    STEP_DETAIL["$id"]="$detail"
}

# ---------------------------------------------------------------------------
# The plan — computed from this host, printed in full, before anything runs.
# ---------------------------------------------------------------------------
show_plan() {
    local id todo=0
    for id in "${STEP_IDS[@]}"; do probe "$id"; [[ "${STEP_STATE[$id]}" == done ]] || todo=$((todo + 1)); done

    printf '\nnemr install — the plan for this machine\n'
    printf '  %s, kernel %s, %s, user %s\n' \
        "$(. /etc/os-release 2>/dev/null && echo "${PRETTY_NAME:-unknown}")" \
        "$(uname -r)" "$(uname -m)" "$USER_NAME"

    head2 "Steps"
    local n=0
    for id in "${STEP_IDS[@]}"; do
        n=$((n + 1))
        case "${STEP_STATE[$id]}" in
            done)    printf '  %2d. %-52s already done\n' "$n" "${STEP_LABEL[$id]}" ;;
            rebuild) printf '  %2d. %-52s check and update\n' "$n" "${STEP_LABEL[$id]}" ;;
            *)       printf '  %2d. %-52s WILL DO\n' "$n" "${STEP_LABEL[$id]}" ;;
        esac
        note "${STEP_DETAIL[$id]}"
    done

    head2 "Files it writes"
    printf '  /usr/local/libexec/nemr-volume                    root:root 0755   (sudo)\n'
    printf '  /etc/sudoers.d/nemr-volume                        root:root 0440   (sudo)\n'
    printf '  /etc/systemd/system/user@.service.d/delegate.conf root:root 0644   (sudo)\n'
    is_wsl2 && printf '  /etc/systemd/system/nemr-mount-propagation.service root:root 0644   (sudo)\n'
    printf '  ~/.config/systemd/user/containerd-rootless.service\n'
    printf '  ~/.config/systemd/user/nemrd.service              (installed, not enabled — the CLI starts it)\n'
    printf '  ~/.local/bin/nemr, nemrd, nemr-cloud + 8 nemr-* symlinks\n'
    printf '  ~/.bashrc                                        one marked block: PATH, CONTAINERD_ADDRESS\n'
    printf '  ~/.local/share/nemr/                             volumes, mounts and this machine'"'"'s login live here\n'
    printf '  %s\n' "$LOG"

    head2 "Privileged actions (each is one sudo command; you may be asked for your password)"
    printf '  apt-get update && apt-get install -y %s\n' "${APT_PACKAGES[*]}"
    printf '  systemctl disable --now containerd.service        (the root daemon must not be running — PRIV-01)\n'
    printf '  usermod --add-subuids/--add-subgids 100000-165535 %s\n' "$USER_NAME"
    printf '  loginctl enable-linger %s\n' "$USER_NAME"
    printf '  install the three root-owned files above; the sudoers file is validated with visudo -c BEFORE it is installed\n'
    printf '  the sudoers grant lets %s run /usr/local/libexec/nemr-volume as root, and nothing else (PRIV-02/03)\n' "$USER_NAME"

    head2 "What it downloads"
    printf '  the Ubuntu archive   the packages above — no third-party apt repository is added (NFR-01)\n'
    printf '  crates.io            the Rust dependencies, to build nemr here from this source\n'
    printf '  ghcr.io              %s, pinned to digest\n' "$BASE_IMAGE"
    printf '                       %s\n' "${BASE_DIGEST:-<none recorded>}"
    printf '                       it is pulled, never built locally: a different digest would export\n'
    printf '                       bundles no other machine could restore (F-126)\n'

    head2 "What it will not do"
    printf '  install Node or Claude Code — a stated prerequisite, detected and never installed (D-13)\n'
    printf '  add a third-party apt repository (NFR-01)\n'
    printf '  write, copy or read a Claude login — you log in with /login inside a session (D-02, F-24)\n'
    printf '  touch a running session, or any project you already have\n'

    if (( todo == 0 )); then
        printf '\nEverything is already done. Running it will re-check each step and verify the host.\n'
    fi
}


# ---------------------------------------------------------------------------
# Actions
# ---------------------------------------------------------------------------
do_step() {
    local id="$1" label="${STEP_LABEL[$1]}" rc=0
    if [[ "${STEP_STATE[$id]}" == done ]]; then
        tick "$label — already done"
        return 0
    fi
    FAILED_STEP="$label"
    case "$id" in
    packages)
        sudo_refresh
        logged_long sudo apt-get update || rc=$?
        (( rc == 0 )) && { logged_long sudo apt-get install -y "${APT_PACKAGES[@]}" || rc=$?; }
        (( rc == 0 )) && tick "$label — installed: ${STEP_DETAIL[$id]#will install: }" ;;
    containerd_off)
        sudo_refresh
        logged sudo systemctl disable --now containerd.service || rc=$?
        (( rc == 0 )) && tick "$label — disabled" ;;
    subids)
        sudo_refresh
        grep -q "^${USER_NAME}:" /etc/subuid || logged sudo usermod --add-subuids 100000-165535 "$USER_NAME" || rc=$?
        grep -q "^${USER_NAME}:" /etc/subgid || logged sudo usermod --add-subgids 100000-165535 "$USER_NAME" || rc=$?
        (( rc == 0 )) && tick "$label — added" ;;
    delegation)
        sudo_refresh
        logged sudo install -D -m 0644 deploy/systemd/delegate.conf \
            /etc/systemd/system/user@.service.d/delegate.conf || rc=$?
        (( rc == 0 )) && { logged sudo systemctl daemon-reload || rc=$?; }
        (( rc == 0 )) && tick "$label — installed" ;;
    linger)
        sudo_refresh
        logged sudo loginctl enable-linger "$USER_NAME" || rc=$?
        (( rc == 0 )) && tick "$label — enabled" ;;
    propagation)
        sudo_refresh
        logged sudo install -D -m 0644 deploy/systemd/nemr-mount-propagation.service \
            /etc/systemd/system/nemr-mount-propagation.service || rc=$?
        (( rc == 0 )) && { logged sudo systemctl daemon-reload || rc=$?; }
        (( rc == 0 )) && { logged sudo systemctl enable nemr-mount-propagation.service || rc=$?; }
        (( rc == 0 )) && { logged sudo mount --make-rshared / || rc=$?; }
        (( rc == 0 )) && tick "$label — installed and live" ;;
    units)
        mkdir -p "$HOME/.config/systemd/user"
        local u
        for u in containerd-rootless nemrd; do
            logged install -D -m 0644 "deploy/systemd/user/$u.service" \
                "$HOME/.config/systemd/user/$u.service" || rc=$?
        done
        (( rc == 0 )) && { logged systemctl --user daemon-reload || rc=$?; }
        (( rc == 0 )) && tick "$label — installed" ;;
    daemons)
        logged_long systemctl --user enable --now containerd-rootless.service || rc=$?
        if (( rc == 0 )); then
            # The shared helper, not a hand-rolled poll (F-95): it explains its
            # own failure. Caught by the wait-discipline gate on 2026-09-10,
            # once this file started backgrounding anything at all.
            wait_for_ready "rootless containerd" 30 \
                test -S "$XDG_RUNTIME_DIR/containerd/containerd.sock" >>"$LOG" 2>&1 || true
            if [[ -S "$XDG_RUNTIME_DIR/containerd/containerd.sock" ]]; then
                tick "$label — answering at \$XDG_RUNTIME_DIR/containerd/containerd.sock"
            else
                journalctl --user -u containerd-rootless.service -n 50 --no-pager >>"$LOG" 2>&1 || true
                rc=1
            fi
        fi ;;
    shellenv)
        {
            printf '\n%s\n' "$PROFILE_MARKER"
            echo 'export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"'
            echo 'export CONTAINERD_ADDRESS="${CONTAINERD_ADDRESS:-$XDG_RUNTIME_DIR/containerd/containerd.sock}"'
            echo 'case ":$PATH:" in *":$HOME/.local/bin:"*) ;; *) export PATH="$HOME/.local/bin:$PATH" ;; esac'
            echo '# <<< nemr environment <<<'
        } >>"$HOME/.bashrc"
        tick "$label — added (open a new shell, or source ~/.bashrc)" ;;
    engine)
        local before="" after=""
        [[ -x "$HOME/.local/bin/nemr" ]] && before="$(sha256sum "$HOME/.local/bin/nemr" | cut -d' ' -f1)"
        logged_long ./scripts/install_engine.sh || rc=$?
        if (( rc == 0 )); then
            after="$(sha256sum "$HOME/.local/bin/nemr" | cut -d' ' -f1)"
            if [[ "$before" == "$after" ]]; then tick "$label — already current"
            else tick "$label — installed (${after:0:12})"; fi
        fi ;;
    helper)
        logged_long env -C deploy/nemr-volume cargo build --release || rc=$?
        if (( rc == 0 )); then
            local built installed=""
            built="$(sha256sum deploy/nemr-volume/target/release/nemr-volume | cut -d' ' -f1)"
            [[ -x /usr/local/libexec/nemr-volume ]] && installed="$(sha256sum /usr/local/libexec/nemr-volume | cut -d' ' -f1)"
            if [[ "$built" == "$installed" && -e /etc/sudoers.d/nemr-volume ]]; then
                tick "$label — already current"
            else
                sudo_refresh
                logged sudo ./scripts/setup_test_host.sh || rc=$?
                (( rc == 0 )) && tick "$label — installed, grant validated with visudo"
            fi
        fi ;;
    client)
        local before="" after=""
        [[ -x "$HOME/.local/bin/nemr-cloud" ]] && before="$(sha256sum "$HOME/.local/bin/nemr-cloud" | cut -d' ' -f1)"
        logged_long ./scripts/install_sync_client.sh || rc=$?
        if (( rc == 0 )); then
            after="$(sha256sum "$HOME/.local/bin/nemr-cloud" | cut -d' ' -f1)"
            if [[ "$before" == "$after" ]]; then tick "$label — already current"
            else tick "$label — installed (${after:0:12})"; fi
        fi ;;
    image)
        logged_long ./scripts/fetch_base_image.sh || rc=$?
        (( rc == 0 )) && tick "$label — at the recorded digest ${BASE_DIGEST:7:12}" ;;
    claude)
        # Detected, never installed (D-13). Not a failure: everything else is
        # finished, and this is the last piece the user provides.
        if have claude; then
            tick "$label — found at $(command -v claude)"
        else
            cross "$label — NOT INSTALLED"
            note "Install Node from nodejs.org or your platform's packaging, then Claude"
            note "Code from its own instructions. nemr does not install it for you (D-13)."
        fi ;;
    verify)
        # An install is done when the host passes, not when commands exit zero.
        # The API round-trip needs a login and this machine may have none yet by
        # design — the login happens with /login inside a session (E-21, F-24).
        if logged_long env NEMR_SKIP_API=1 ./scripts/e2e_smoke_test.sh; then
            tick "$label — passed: create, start, attach, stop, delete, host left clean"
        else
            rc=1
        fi ;;
    esac

    if (( rc != 0 )); then cross "$label"; exit "$rc"; fi
    FAILED_STEP=""
    return 0
}

# The reboot gate, surfaced rather than hidden: delegation applies when
# user@.service restarts, and restarting it kills the session that asks.
reboot_gate() {
    local delegated=""
    [[ -r "$DELEGATED_CONTROLLERS_PATH" ]] && delegated="$(cat "$DELEGATED_CONTROLLERS_PATH")"
    [[ " $delegated " == *" cpu "* ]] && return 0
    FAILED_STEP=""
    cat >&2 <<EOF

  cgroup delegation is configured, but it is not in effect yet.
      expected: cpu among this user's delegated controllers
      found:    ${delegated:-<the user slice is not readable>}

  It applies when user@${USER_ID}.service restarts, and restarting that kills the
  session asking for it — so in practice this is a reboot.

      sudo reboot

  Then run this again. It will skip everything above and carry on from here.

EOF
    exit 3
}

# ---------------------------------------------------------------------------
# Run
# ---------------------------------------------------------------------------
steps_trap
preflight
show_plan
nemr_consent "$YES" "./scripts/install.sh"

open_log
head2 "Installing"

# The live region (F-27): the step lines on the left, redrawn in place, and the
# cat looping beside them — one writer, nothing scrolling. Below its thresholds
# (129 columns, 17 rows) this returns non-zero and the block prints as it always
# did, line by line.
REGION_STATE="$LOG_DIR/region.$$"
_NEMR_REGION_STATE_PATH="$REGION_STATE"   # so sudo_refresh can reopen it
# Nothing may prompt while the region owns the screen, so the password is taken
# BEFORE it opens and the timestamp is kept warm underneath it. Only when a
# privileged step is actually pending — a run where everything is already done
# asks for nothing, as before.
PRIVILEGED_PENDING=0
for id in "${STEP_IDS[@]}"; do
    case "$id" in
        packages|containerd_off|subids|delegation|linger|propagation|helper)
            # Only a step that WILL do privileged work. A "check and update"
            # step (the helper) needs sudo just when its hash turns out to
            # differ, and asking for a password that may not be needed is a
            # regression on today's behaviour. That rare case is handled by
            # sudo_refresh closing the region around the prompt.
            [[ "${STEP_STATE[$id]}" == todo ]] && PRIVILEGED_PENDING=1 ;;
    esac
done
if nemr_region_enabled && (( PRIVILEGED_PENDING )); then
    sudo_refresh
fi
if nemr_region_start "$REGION_STATE"; then
    _S_REGION=1
    # Seeded with every step, so the whole list is visible from the first frame
    # and the user never scrolls to watch it.
    for id in "${STEP_IDS[@]}"; do _S_LINES+=("  · ${STEP_LABEL[$id]}"); done
    nemr_region_publish "${_S_LINES[@]}"
    if (( PRIVILEGED_PENDING )); then
        ( while kill -0 $$ 2>/dev/null; do sudo -n true 2>/dev/null || exit 0; sleep 45; done ) &
        SUDO_KEEPALIVE=$!
    fi
fi

_S_IDX=0
for id in "${STEP_IDS[@]}"; do
    do_step "$id"
    _S_IDX=$((_S_IDX + 1))
    [[ "$id" == "linger" ]] && reboot_gate
done

# ONE region for the WHOLE run. It used to stop here, and verification then drew
# its own full-width cat below the resolved list, which scrolled the block away:
# the layout was abandoned half way through the very run it exists for. The
# smoke test and the Claude Code check are steps like any other now.
if (( _S_REGION )); then
    nemr_region_stop            # resolves to the final list; the cat is gone
    _S_REGION=0
    [[ -n "${SUDO_KEEPALIVE:-}" ]] && { kill "$SUDO_KEEPALIVE" 2>/dev/null; wait "$SUDO_KEEPALIVE" 2>/dev/null; }
    rm -f "$REGION_STATE"
    flush_notes
fi

cat <<EOF

nemr is installed.

  nemr create myproject     make a session and attach to it
  nemr ui                   the same thing in a browser

Inside a session, run /login the first time — that logs this machine in, and
the login stays here (it is never copied into a bundle or to another machine).
EOF
