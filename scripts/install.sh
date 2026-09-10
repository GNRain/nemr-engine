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
VERBOSE=0
usage() {
    cat <<'USAGE'
nemr install — the engine, the helper, the base image and the CLI, on this machine.

  ./scripts/install.sh          show the plan, ask once, install
  ./scripts/install.sh --yes    accept the plan without the question
  ./scripts/install.sh --quiet  no animation; step lines only
  ./scripts/install.sh --verbose  every line of today's output: the full plan,
                                every step's detail, appended as it happens —
                                no live screen
  ./scripts/install.sh --help   this

It is safe to run twice: a step already done says so instead of being redone.
Running your own sync server instead? That is scripts/install_server.sh (D-14).
USAGE
}
while (($#)); do
    case "$1" in
        -y|--yes)   YES=1 ;;
        -q|--quiet) NEMR_CAT=0 ;;
        -v|--verbose) VERBOSE=1; _S_VERBOSE=1; NEMR_CAT=0 ;;
        -h|--help)  usage; exit 0 ;;
        *) printf 'install.sh: unknown option %s\n\n' "$1"; usage; exit 2 ;;
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

DEST_BIN="${NEMR_INSTALLED_BIN:-$HOME/.local/bin/nemr}"
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
        RESULT_STATE=handled
        printf 'Run this as your normal user, not root.\n'
        printf 'The stack is rootless (PRIV-01); the script asks for sudo where it needs it.\n'
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
        RESULT_STATE=handled
        printf '\n%snemr cannot be installed on this machine yet — nothing has been changed.%s\n\n' \
            "$_S_RED" "$_S_RESET"
        local m
        for m in "${MISSING[@]}"; do printf '  needs: %s\n\n' "$m"; done
        printf 'Fix these and run this again. PREREQUISITES.md explains why each one matters.\n'
        exit 1
    fi
}

# ---------------------------------------------------------------------------
# The steps. Each has a probe (is it already done?) and an action.
# ---------------------------------------------------------------------------
STEP_IDS=()
declare -A STEP_LABEL=() STEP_PLAN=() STEP_STATE=() STEP_DETAIL=()

# <id> <what it is doing, in plain words> <the full name, for the plan>
#
# Two names on purpose. The screen says what is happening right now — "building
# the engine" — and nothing else; the plan, printed in full before the question
# that consents to the run, is where a step is explained (D-14). The default
# REPORTS THE RESULT; it does not narrate the work (the Product Owner,
# 2026-09-10, comparing this to Claude Code's own installer: four lines).
step_def() { STEP_IDS+=("$1"); STEP_LABEL["$1"]="$2"; STEP_PLAN["$1"]="${3:-$2}"; }

step_def packages        "installing packages" "packages from the Ubuntu archive"
step_def containerd_off  "disabling the system containerd" "the system-wide root containerd, disabled"
is_wsl2 && step_def propagation     "making the mount shared" "shared mount propagation (WSL2)"
step_def subids          "adding subuid/subgid ranges" "subuid/subgid ranges for $USER_NAME"
step_def delegation      "delegating cgroup controllers" "cgroup v2 controller delegation"
step_def linger          "enabling lingering" "lingering, so the user manager runs without a login"
step_def units           "installing the user units" "the rootless containerd and nemrd user units"
step_def daemons         "starting rootless containerd" "rootless containerd, running"
step_def shellenv        "updating the shell environment" "PATH and CONTAINERD_ADDRESS in ~/.bashrc"
step_def engine          "building the engine" "the engine, built and installed (nemr, nemrd)"
step_def helper          "installing the privileged helper" "the privileged volume helper and its sudoers grant"
step_def client          "building the client" "the client CLI (nemr ui, push, pull)"
step_def image           "pulling the base image" "the base image $BASE_IMAGE"
step_def claude          "checking for Claude Code" "Claude Code, a prerequisite (detected, never installed)"
step_def verify          "running the smoke test" "the host passes the smoke test"

missing_packages() {
    local p out=()
    for p in "${APT_PACKAGES[@]}"; do
        dpkg-query -W -f='${Status}' "$p" 2>/dev/null | grep -q "^install ok installed$" || out+=("$p")
    done
    printf '%s\n' "${out[*]}"
}

probe() {
    local id="$1" state=done detail=""
    if [[ -n "${NEMR_TEST_STEP_STUB:-}" ]]; then
        STEP_STATE["$id"]=todo
        STEP_DETAIL["$id"]="will do this on a machine that has none of it"
        return 0
    fi
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
            done)    printf '  %2d. %-52s already done\n' "$n" "${STEP_PLAN[$id]}" ;;
            rebuild) printf '  %2d. %-52s check and update\n' "$n" "${STEP_PLAN[$id]}" ;;
            *)       printf '  %2d. %-52s WILL DO\n' "$n" "${STEP_PLAN[$id]}" ;;
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

    # TEST-ONLY seams, both unset in every real run.
    #
    #   NEMR_TEST_STEP_STUB   report what a FIRST install reports, without doing
    #                         any of it. The layout has only ever been captured
    #                         on idempotent runs, which is why the first-run
    #                         line that tears it was never seen (F-29).
    #   NEMR_TEST_FAIL_STEP   fail this step, to exercise the failure path.
    if [[ "${NEMR_TEST_FAIL_STEP:-}" == "$id" ]]; then
        step_result fail "FAILED" "forced by NEMR_TEST_FAIL_STEP — see $LOG"
        step_diagnosis "$id"
        exit 1
    fi
    #   NEMR_TEST_FORCE_ABRUPT  exit with NOTHING recorded — no step_result, no
    #                           RESULT_STATE — while the region owns the screen.
    #                           The shape of an exit nobody wired; proves the
    #                           backstop prints rather than the run going silent.
    if [[ "${NEMR_TEST_FORCE_ABRUPT:-}" == "$id" ]]; then
        exit 1
    fi
    if [[ -n "${NEMR_TEST_STEP_STUB:-}" ]]; then
        # A number sets the per-step delay, so an interrupt can be aimed.
        if [[ "$NEMR_TEST_STEP_STUB" =~ ^[0-9.]+$ ]]; then sleep "$NEMR_TEST_STEP_STUB"
        else sleep 0.35; fi
        case "$id" in
            packages) step_result done "new" "installed: ${APT_PACKAGES[*]}" ;;
            claude)   step_result warn "absent" "NOT INSTALLED — install Node, then Claude Code (D-13)" ;;
            image)    step_result done "new" "pulled $BASE_IMAGE at ${BASE_DIGEST:7:12}" ;;
            verify)   step_result done "ok" "passed: create, start, attach, stop, delete, host left clean" ;;
            *)        step_result done "new" "${STEP_DETAIL[$id]#will }" ;;
        esac
        return 0
    fi

    if [[ "${STEP_STATE[$id]}" == done ]]; then
        step_result done "done" "already done"
        return 0
    fi
    FAILED_STEP="$label"
    case "$id" in
    packages)
        sudo_refresh
        logged_long sudo apt-get update || rc=$?
        (( rc == 0 )) && { logged_long sudo apt-get install -y "${APT_PACKAGES[@]}" || rc=$?; }
        (( rc == 0 )) && step_result done "new" "installed: ${STEP_DETAIL[$id]#will install: }" ;;
    containerd_off)
        sudo_refresh
        logged sudo systemctl disable --now containerd.service || rc=$?
        (( rc == 0 )) && step_result done "new" "disabled" ;;
    subids)
        sudo_refresh
        grep -q "^${USER_NAME}:" /etc/subuid || logged sudo usermod --add-subuids 100000-165535 "$USER_NAME" || rc=$?
        grep -q "^${USER_NAME}:" /etc/subgid || logged sudo usermod --add-subgids 100000-165535 "$USER_NAME" || rc=$?
        (( rc == 0 )) && step_result done "new" "added" ;;
    delegation)
        sudo_refresh
        logged sudo install -D -m 0644 deploy/systemd/delegate.conf \
            /etc/systemd/system/user@.service.d/delegate.conf || rc=$?
        (( rc == 0 )) && { logged sudo systemctl daemon-reload || rc=$?; }
        (( rc == 0 )) && step_result done "new" "installed" ;;
    linger)
        sudo_refresh
        logged sudo loginctl enable-linger "$USER_NAME" || rc=$?
        (( rc == 0 )) && step_result done "new" "enabled" ;;
    propagation)
        sudo_refresh
        logged sudo install -D -m 0644 deploy/systemd/nemr-mount-propagation.service \
            /etc/systemd/system/nemr-mount-propagation.service || rc=$?
        (( rc == 0 )) && { logged sudo systemctl daemon-reload || rc=$?; }
        (( rc == 0 )) && { logged sudo systemctl enable nemr-mount-propagation.service || rc=$?; }
        (( rc == 0 )) && { logged sudo mount --make-rshared / || rc=$?; }
        (( rc == 0 )) && step_result done "new" "installed and live" ;;
    units)
        mkdir -p "$HOME/.config/systemd/user"
        local u
        for u in containerd-rootless nemrd; do
            logged install -D -m 0644 "deploy/systemd/user/$u.service" \
                "$HOME/.config/systemd/user/$u.service" || rc=$?
        done
        (( rc == 0 )) && { logged systemctl --user daemon-reload || rc=$?; }
        (( rc == 0 )) && step_result done "new" "installed" ;;
    daemons)
        logged_long systemctl --user enable --now containerd-rootless.service || rc=$?
        if (( rc == 0 )); then
            # The shared helper, not a hand-rolled poll (F-95): it explains its
            # own failure. Caught by the wait-discipline gate on 2026-09-10,
            # once this file started backgrounding anything at all.
            wait_for_ready "rootless containerd" 30 \
                test -S "$XDG_RUNTIME_DIR/containerd/containerd.sock" >>"$LOG" 2>&1 || true
            if [[ -S "$XDG_RUNTIME_DIR/containerd/containerd.sock" ]]; then
                step_result done "new" "answering at \$XDG_RUNTIME_DIR/containerd/containerd.sock"
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
        step_result done "new" "added — open a new shell, or source ~/.bashrc" ;;
    engine)
        local before="" after=""
        [[ -x "$HOME/.local/bin/nemr" ]] && before="$(sha256sum "$HOME/.local/bin/nemr" | cut -d' ' -f1)"
        logged_long ./scripts/install_engine.sh || rc=$?
        if (( rc == 0 )); then
            after="$(sha256sum "$HOME/.local/bin/nemr" | cut -d' ' -f1)"
            if [[ "$before" == "$after" ]]; then step_result done "done" "already current"
            else step_result done "new" "installed (${after:0:12})"; fi
        fi ;;
    helper)
        logged_long env -C deploy/nemr-volume cargo build --release || rc=$?
        if (( rc == 0 )); then
            local built installed=""
            built="$(sha256sum deploy/nemr-volume/target/release/nemr-volume | cut -d' ' -f1)"
            [[ -x /usr/local/libexec/nemr-volume ]] && installed="$(sha256sum /usr/local/libexec/nemr-volume | cut -d' ' -f1)"
            if [[ "$built" == "$installed" && -e /etc/sudoers.d/nemr-volume ]]; then
                step_result done "done" "already current"
            else
                sudo_refresh
                logged sudo ./scripts/setup_test_host.sh || rc=$?
                (( rc == 0 )) && step_result done "new" "installed, grant validated with visudo"
            fi
        fi ;;
    client)
        local before="" after=""
        [[ -x "$HOME/.local/bin/nemr-cloud" ]] && before="$(sha256sum "$HOME/.local/bin/nemr-cloud" | cut -d' ' -f1)"
        logged_long ./scripts/install_sync_client.sh || rc=$?
        if (( rc == 0 )); then
            after="$(sha256sum "$HOME/.local/bin/nemr-cloud" | cut -d' ' -f1)"
            if [[ "$before" == "$after" ]]; then step_result done "done" "already current"
            else step_result done "new" "installed (${after:0:12})"; fi
        fi ;;
    image)
        logged_long ./scripts/fetch_base_image.sh || rc=$?
        (( rc == 0 )) && step_result done "ok" "at the recorded digest ${BASE_DIGEST:7:12}" ;;
    claude)
        # Detected, never installed (D-13). Not a failure: everything else is
        # finished, and this is the last piece the user provides.
        if have claude; then
            step_result done "ok" "found at $(command -v claude)"
        else
            # A warning, not a failure: everything else is finished, and this
            # is the piece the user provides (D-13).
            step_result warn "absent" "NOT INSTALLED — install Node, then Claude Code from its own instructions (D-13)"
        fi ;;
    verify)
        # An install is done when the host passes, not when commands exit zero.
        # The API round-trip needs a login and this machine may have none yet by
        # design — the login happens with /login inside a session (E-21, F-24).
        if logged_long env NEMR_SKIP_API=1 ./scripts/e2e_smoke_test.sh; then
            step_result done "ok" "passed: create, start, attach, stop, delete, host left clean"
        else
            rc=1
        fi ;;
    esac

    if (( rc != 0 )); then
        step_result fail "FAILED" "see $LOG"
        step_diagnosis "$id"
        exit "$rc"
    fi
    FAILED_STEP=""
    return 0
}

# WHY IT STOPPED, in plain words, and the command that fixes it.
#
# Two sources: what the step itself knows, and a handful of causes that can be
# read out of the log. Anything unrecognised says so and points at the log —
# a guess dressed as a diagnosis is worse than "read this".
step_diagnosis() {
    local id="$1" because fix tail
    tail="$(tail -60 "$LOG" 2>/dev/null || true)"
    case "$tail" in
        *"iptables"*"not found"*|*"iptables: command not found"*)
            because="iptables is missing"; fix="sudo apt install iptables, then run this again" ;;
        *"No space left on device"*)
            because="the disk is full"; fix="free some space, then run this again" ;;
        *"Could not resolve host"*|*"Temporary failure in name resolution"*)
            because="this machine cannot reach the network"; fix="restore network access, then run this again" ;;
        *"Permission denied"*"sudoers"*|*"is not in the sudoers file"*)
            because="this account may not use sudo"; fix="ask an administrator for sudo, then run this again" ;;
        *"denied"*"ghcr.io"*|*"unauthorized"*)
            because="ghcr.io refused the base image"; fix="check network access to ghcr.io, then run this again" ;;
        *)
            case "$id" in
                packages)  because="apt could not install the packages"; fix="read the log below, fix the cause, then run this again" ;;
                engine|client) because="the build failed"; fix="cargo build --release, read the error, then run this again" ;;
                helper)    because="the privileged helper would not install"; fix="sudo ./scripts/setup_test_host.sh, then run this again" ;;
                image)     because="the base image could not be obtained"; fix="./scripts/fetch_base_image.sh, then run this again" ;;
                verify)    because="the host did not pass its own smoke test"; fix="read the log below — it names the assertion that failed" ;;
                *)         because="the step did not succeed"; fix="read the log below, then run this again" ;;
            esac ;;
    esac
    RESULT_FAILED_STEP="${STEP_LABEL[$id]}"
    RESULT_BECAUSE="$because"
    RESULT_FIX="$fix"
}

# The reboot gate, surfaced rather than hidden: delegation applies when
# user@.service restarts, and restarting it kills the session that asks.
reboot_gate() {
    # A test seam so the paused outcome can be exercised where delegation IS
    # live (this reference host); unset in every real run.
    if [[ -z "${NEMR_TEST_FORCE_REBOOT_GATE:-}" ]]; then
        local delegated=""
        [[ -r "$DELEGATED_CONTROLLERS_PATH" ]] && delegated="$(cat "$DELEGATED_CONTROLLERS_PATH")"
        [[ " $delegated " == *" cpu "* ]] && return 0
    fi
    # NOT a failure, and NOT the script's job to print here: the region may own
    # the screen. Record the third outcome and exit; the single authority
    # (_steps_cleanup) tears the region down and prints it. Printing a heredoc
    # to stderr from here, while clearing FAILED_STEP, was exactly the first
    # install that produced no output at all (F-32).
    RESULT_STATE=paused
    RESULT_PAUSED_WHY="cgroup delegation applies only after the user manager restarts"
    exit 3
}

# ---------------------------------------------------------------------------
# Run
# ---------------------------------------------------------------------------
steps_trap
preflight

# THE PLAN IS THE CONSENT, and only that. An interactive run prints it in full
# and asks; --verbose prints it because --verbose prints everything; a run that
# already said --yes has consented, so it gets the screen and not the recital.
# That recital was most of the hundred lines this used to print (the Product
# Owner, 2026-09-10: "ours narrates the work where theirs reports the result").
if (( ! YES )) || (( VERBOSE )); then
    show_plan
else
    for id in "${STEP_IDS[@]}"; do probe "$id"; done
fi
nemr_consent "$YES" "./scripts/install.sh"

open_log
(( VERBOSE )) && head2 "Installing"

# The live region (F-27): the step lines on the left, redrawn in place, and the
# cat looping beside them — one writer, nothing scrolling. Below its thresholds
# (129 columns, 17 rows) this returns non-zero and the block prints as it always
# did, line by line.
# Strays from dead runs, swept before this run makes its own (F-34). A crash or
# a kill -9 cannot run the cleanup trap, so its region./step. files would sit in
# the state dir forever — the Product Owner found a pair per run. A file whose
# pid is no longer alive is nobody's, so it goes.
for _stray in "$LOG_DIR"/region.* "$LOG_DIR"/step.*; do
    [[ -e "$_stray" ]] || continue
    _spid="${_stray##*.}"
    [[ "$_spid" =~ ^[0-9]+$ ]] && ! kill -0 "$_spid" 2>/dev/null && rm -f "$_stray" 2>/dev/null || true
done

REGION_STATE="$LOG_DIR/region.$$"
# The current step's own output: the pane tails it, and step_result folds it
# into the log when the step ends.
STEP_OUT="$LOG_DIR/step.$$"
: >"$STEP_OUT" 2>/dev/null || STEP_OUT=""
# Registered with the single authority, which removes them on EVERY exit —
# success, failure, or Ctrl-C (F-34).
STEPS_TEMP_FILES=("$REGION_STATE" "$STEP_OUT")
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
# The stub does no privileged work, so it must not ask for a password.
[[ -n "${NEMR_TEST_STEP_STUB:-}" ]] && PRIVILEGED_PENDING=0
#   NEMR_TEST_FORCE_SUDO_ASK  ask sudo even under the stub (the test puts a
#                             refusing `sudo` first on PATH to drive the
#                             refusal path; the real one is never touched).
[[ -n "${NEMR_TEST_FORCE_SUDO_ASK:-}" ]] && PRIVILEGED_PENDING=1
if nemr_region_enabled && (( PRIVILEGED_PENDING )); then
    sudo_refresh
fi
# WHY THE SCREEN IS OR IS NOT LIVE — written to the log on EVERY run, because a
# report that "the two-column region did not render" could not be answered from
# here: the log was identical either way, and every one of the refusals was
# silent (measured, 2026-09-10). This block is what makes the next such report
# answerable by reading the log the reporter already has.
if (( VERBOSE )); then _NEMR_SCREEN_WHY="--verbose"; fi
# THE DECISION IS MADE HERE, on the real stdout — never inside the block below.
# It used to be made inside it, where the redirect to the log file makes
# `[[ -t 1 ]]` false by construction, so every run that ever wrote this block
# answered "NO — not-a-tty", including the hundreds that plainly drew a screen:
# 399 logs on this machine, none of them saying yes. A control that can only
# give one answer is worse than no control, because it gets read.
SCREEN_LIVE=0
(( ! VERBOSE )) && nemr_region_enabled && SCREEN_LIVE=1
_screen_size="$(nemr_term_size 2>/dev/null || true)"
_screen_by="$(stty size </dev/tty >/dev/null 2>&1 && echo 'stty /dev/tty' || echo 'tput/terminfo')"
# Also measured out here: inside the block, stdout is the log file.
# NOT in a command substitution: inside `$( )` stdout is a pipe, so `-t 1` is
# false there whatever the real stdout is — the same mistake as asking the
# question inside the block's own redirect, one line further down.
if [[ -t 1 ]]; then _screen_tty=yes; else _screen_tty=no; fi
{
    printf '\n--- install screen\n'
    if (( SCREEN_LIVE )); then
        printf '    live:      yes\n'
    else
        printf '    live:      NO — %s\n' "${_NEMR_SCREEN_WHY:-unknown}"
    fi
    printf '    measured:  %s   by: %s\n' \
        "${NEMR_SCREEN_MEASURED:-${_screen_size:+${_screen_size#* } cols x ${_screen_size%% *} rows}}" \
        "$_screen_by"
    printf '    needs:     %s cols x %s rows\n' "$NEMR_REGION_MIN_COLS" "$NEMR_REGION_MIN_ROWS"
    printf '    fitted:    left %s + gap %s + cat %s (right edge)   pane inner %s\n' \
        "$NEMR_REGION_LEFT_COLS" "$NEMR_REGION_GAP" "$NEMR_REGION_CAT_COLS" "$_nemr_pane_inner"
    printf '    resize:    not supported mid-run — the width is read once, at the start\n'
    printf '    terminal:  TERM=%s  tty=%s  NO_COLOR=%s  NEMR_CAT=%s\n' \
        "${TERM:-unset}" "$_screen_tty" \
        "${NO_COLOR:-unset}" "${NEMR_CAT:-unset}"
    printf '    host:      %s  TMPDIR=%s\n' \
        "$(is_wsl2 && echo WSL2 || echo linux)" "${TMPDIR:-/tmp}"
    printf '    note:      the cursor query (DSR) does not gate this — it only\n'
    printf '               decides how far to scroll the screen into place\n\n'
} >>"$LOG" 2>/dev/null || true

if (( SCREEN_LIVE )) && nemr_region_start "$REGION_STATE" "$STEP_OUT"; then
    _S_REGION=1
    # Seeded with every step, so the whole list is on screen from the first
    # frame: what is finished, what is running, what is still to come.
    region_labels=()
    for id in "${STEP_IDS[@]}"; do region_labels+=("${STEP_LABEL[$id]}"); done
    steps_seed "${region_labels[@]}"
    _s_republish
    if (( PRIVILEGED_PENDING )); then
        ( while kill -0 $$ 2>/dev/null; do sudo -n true 2>/dev/null || exit 0; sleep 45; done ) &
        SUDO_KEEPALIVE=$!
    fi
else
    # One line, and only when the reason is the terminal rather than the user's
    # own instruction: --quiet, --verbose and a pipe are choices, and a pipe is
    # where noise costs most. A successful default run gains nothing.
    case "${_NEMR_SCREEN_WHY:-}" in
        ""|"--verbose"|quiet*|not-a-tty*) ;;
        # Short enough to fit the terminal that just refused the screen: at 70
        # columns a line naming the log path wraps, which is a poor first
        # impression from the very message explaining a layout problem.
        *) printf '  %sno live screen — %s (see the log)%s\n\n' \
               "$_S_DIM" "$_NEMR_SCREEN_WHY" "$_S_RESET" ;;
    esac
fi

# In append-only mode the list still exists — step_result prints from it.
(( _S_REGION )) || { region_labels=(); for id in "${STEP_IDS[@]}"; do region_labels+=("${STEP_LABEL[$id]}"); done; steps_seed "${region_labels[@]}"; }

_S_IDX=0
for id in "${STEP_IDS[@]}"; do
    step_begin
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
    flush_notes
fi

# The result is RECORDED here and PRINTED by the single authority (F-32), so it
# cannot be skipped by an early return or erased by the region. Everything the
# run did is in the log; --verbose prints it as it happens.
NEMR_VERSION="$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)"
have claude || CLAUDE_MISSING=1
RESULT_OK_VERSION="${NEMR_VERSION:-unknown}"
RESULT_OK_DEST="${DEST_BIN/#$HOME/\~}"
RESULT_OK_CLAUDE="${CLAUDE_MISSING:-}"
RESULT_STATE=ok
