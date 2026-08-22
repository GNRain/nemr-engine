#!/usr/bin/env bash
#
# Nemr — end-to-end smoke test (NEMR-SPEC-001, Milestone 7).
#
# Drives the complete project lifecycle non-interactively and asserts at every
# step: create -> start -> attach (scripted Claude Code invocation) -> stop ->
# start -> list -> delete, plus reboot survival (VOL-06) and host cleanliness.
#
# This is the standing regression test for all engine changes (AC-7.2). Run it
# before merging anything that touches the engine, the privileged helper, or the
# base image.
#
#   ./scripts/e2e_smoke_test.sh            # full run
#   NEMR_SKIP_API=1 ./scripts/e2e_smoke_test.sh   # skip the API round-trip
#
# Exit codes: 0 all steps passed; 1 a step failed; 2 prerequisites missing.
#
# It deliberately asserts on *host* state — loop devices, mount entries, backing
# files — rather than only on the engine's own output. The engine agreeing with
# itself is not evidence; several real defects in this project were found only
# because host state disagreed with what the engine reported.

set -euo pipefail

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------

# A dedicated project name, suffixed with the PID so a stale run cannot collide
# with a live one and so the script never touches a real project.
readonly PROJECT="e2e-smoke-$$"
readonly SIZE="500MB"
readonly NEMR="${NEMR_BIN:-nemr}"
readonly VOLUME_DIR="${HOME}/.local/share/nemr/volumes"
readonly MOUNT_DIR="${HOME}/.local/share/nemr/mounts"
readonly HELPER="/usr/local/libexec/nemr-volume"

STEP=0
FAILED=0

# ---------------------------------------------------------------------------
# Output helpers
# ---------------------------------------------------------------------------

step()  { STEP=$((STEP + 1)); printf '\n\033[1m[%02d] %s\033[0m\n' "$STEP" "$*"; }
ok()    { printf '     \033[32mok\033[0m   %s\n' "$*"; }
fail()  { printf '     \033[31mFAIL\033[0m %s\n' "$*"; FAILED=$((FAILED + 1)); }
info()  { printf '          %s\n' "$*"; }

# Assert a condition, reporting either way. Never aborts on its own — the run
# continues so one failure does not hide the state of everything after it.
assert() {
    local description="$1"; shift
    if "$@"; then ok "$description"; else fail "$description"; fi
}

assert_eq() {
    local description="$1" expected="$2" actual="$3"
    if [[ "$expected" == "$actual" ]]; then
        ok "$description"
    else
        fail "$description (expected '$expected', got '$actual')"
    fi
}

# ---------------------------------------------------------------------------
# Cleanup
# ---------------------------------------------------------------------------

# Runs on every exit path, including failure and interrupt. A smoke test that
# leaks a loop device on failure would poison every subsequent run.
cleanup() {
    local exit_code=$?
    if $NEMR list 2>/dev/null | grep -q "^${PROJECT} "; then
        printf '\n     cleaning up %s\n' "$PROJECT"
        $NEMR delete "$PROJECT" --yes >/dev/null 2>&1 || true
    fi
    # Belt and braces: release anything the engine may have left behind.
    sudo -n "$HELPER" unmount "$PROJECT" >/dev/null 2>&1 || true
    rm -f "${VOLUME_DIR}/${PROJECT}.img" 2>/dev/null || true
    rmdir "${MOUNT_DIR}/${PROJECT}" 2>/dev/null || true
    exit $exit_code
}
trap cleanup EXIT INT TERM

# ---------------------------------------------------------------------------
# Host-state probes — the authority, independent of what the engine reports
# ---------------------------------------------------------------------------

loop_devices_for() { losetup -a 2>/dev/null | grep -c "${VOLUME_DIR}/${1}\.img" || true; }
mount_entries_for() { grep -c "${MOUNT_DIR}/${1}\b" /proc/self/mountinfo 2>/dev/null || true; }

# Run the piped-in commands inside the project's container.
in_container() { $NEMR attach "$PROJECT" 2>/dev/null; }

# ---------------------------------------------------------------------------
# 0. Prerequisites
# ---------------------------------------------------------------------------

step "Prerequisites (Section 3.3)"

missing=0
for binary in containerd runc ctr losetup mkfs.ext4; do
    command -v "$binary" >/dev/null || { fail "$binary not found"; missing=1; }
done
command -v "$NEMR" >/dev/null || { fail "$NEMR not found — run ./scripts/install_engine.sh"; missing=1; }

# F-62: this script exercises whatever `nemr` is on PATH. Without a freshness
# check it can pass against a binary built from a commit that no longer exists
# and be reported as a milestone closing. The gate lives in the Rust harness so
# there is one implementation, not two that can drift.
if command -v "$NEMR" >/dev/null; then
    if ! cargo test --test regression the_installed_engine_matches_its_source \
         --quiet >/dev/null 2>&1; then
        fail "the installed nemr is not this source — run ./scripts/install_engine.sh"
        cargo test --test regression the_installed_engine_matches_its_source 2>&1 \
            | sed -n '/is not this source/,/^$/p' >&2
        missing=1
    fi
fi
[[ -x "$HELPER" ]] || { fail "privileged helper missing at $HELPER — see PREREQUISITES.md"; missing=1; }

if ! systemctl --user is-active --quiet containerd-rootless.service; then
    fail "rootless containerd is not running (systemctl --user status containerd-rootless.service)"
    missing=1
fi

# The helper exits 1 on a bare invocation (it prints usage), so success is
# "it ran and said usage", not "it exited 0". Capture the output first rather
# than piping: under `set -o pipefail` a pipeline reports the helper's exit
# status even when grep matches, which would fail this check on a perfectly
# well-configured host.
helper_output=$(sudo -n "$HELPER" 2>&1 || true)
if ! grep -q usage <<<"$helper_output"; then
    fail "cannot invoke $HELPER via sudo -n — is /etc/sudoers.d/nemr-volume installed?"
    info "got: ${helper_output:-<no output>}"
    missing=1
fi

delegated=$(cat /sys/fs/cgroup/user.slice/user-"$(id -u)".slice/user@"$(id -u)".service/cgroup.controllers 2>/dev/null || echo "")
if [[ "$delegated" != *cpu* ]]; then
    fail "cgroup v2 cpu controller not delegated — see PREREQUISITES.md Step 2a"
    missing=1
fi

if [[ $missing -ne 0 ]]; then
    printf '\n\033[31mPrerequisites missing. See PREREQUISITES.md.\033[0m\n'
    exit 2
fi
ok "containerd, runc, helper, sudoers grant, cgroup delegation"
export CONTAINERD_ADDRESS="${XDG_RUNTIME_DIR}/containerd/containerd.sock"

# ---------------------------------------------------------------------------
# 1. Create
# ---------------------------------------------------------------------------

step "create — $PROJECT ($SIZE)"

$NEMR create "$PROJECT" --size "$SIZE" >/dev/null
assert "container record exists in containerd" \
    bash -c "ctr containers list 2>/dev/null | grep -q 'nemr-${PROJECT}'"
assert_eq "backing file allocated" "524288000" "$(stat -c %s "${VOLUME_DIR}/${PROJECT}.img")"
assert_eq "loop device attached" "1" "$(loop_devices_for "$PROJECT")"
assert_eq "volume mounted" "1" "$(mount_entries_for "$PROJECT")"

# Sparse: the file must not actually consume its full apparent size.
actual_blocks=$(stat -c %b "${VOLUME_DIR}/${PROJECT}.img")
assert "backing file is sparse ($((actual_blocks / 2)) KiB on disk of 500 MiB apparent)" \
    test "$actual_blocks" -lt 262144

assert "stopped-but-ready: no task yet" \
    bash -c "! ctr tasks list 2>/dev/null | grep -q 'nemr-${PROJECT}'"

# ---------------------------------------------------------------------------
# 2. Start
# ---------------------------------------------------------------------------

step "start"

$NEMR start "$PROJECT" >/dev/null
assert "task is running" \
    bash -c "ctr tasks list 2>/dev/null | grep 'nemr-${PROJECT}' | grep -q RUNNING"
assert "cgroup scope created under the delegated user slice" \
    bash -c "systemctl --user list-units --type=scope --all 2>/dev/null | grep -q 'nemr-${PROJECT}.scope'"

# ---------------------------------------------------------------------------
# 3. Attach — the container's own view
# ---------------------------------------------------------------------------

step "attach — shell, volume, credentials"

workdir=$(echo 'pwd' | in_container | tr -d '\r' | grep -oE '^/workspace$' | head -1 || true)
assert_eq "shell starts in /workspace" "/workspace" "$workdir"

fstype=$(echo 'stat -f -c %T /workspace' | in_container | tr -d '\r' | grep -oE '^ext2/ext3$|^ext4$' | head -1 || true)
assert "the volume, not the host filesystem, is mounted at /workspace (got '${fstype:-none}')" \
    bash -c "[[ -n '$fstype' ]]"

quota=$(echo 'df -B1 --output=size /workspace | tail -1' | in_container | tr -dc '0-9\n' | grep -E '^[0-9]{6,}$' | head -1 || true)
assert "quota in force inside the container (${quota:-unknown} bytes < 524288000)" \
    bash -c "[[ -n '$quota' && '$quota' -le 524288000 ]]"

creds=$(echo 'test -r /root/.claude/.credentials.json && echo READABLE' | in_container | tr -d '\r' | grep -c READABLE || true)
assert_eq "credentials mounted (AUTH-02)" "1" "$creds"

readonly_creds=$(echo 'touch /root/.claude/.credentials.json 2>&1 | grep -c "Read-only"' | in_container | tr -dc '0-9\n' | grep -E '^[0-9]+$' | head -1 || true)
assert_eq "credentials are read-only (AUTH-02)" "1" "$readonly_creds"

# ---------------------------------------------------------------------------
# 4. Scripted Claude Code invocation
# ---------------------------------------------------------------------------

step "attach — scripted Claude Code invocation"

version=$(echo 'claude --version' | in_container | tr -d '\r' | grep -oE '^[0-9]+\.[0-9]+\.[0-9]+' | head -1 || true)
assert "Claude Code CLI present (${version:-not found})" bash -c "[[ -n '$version' ]]"

if [[ "${NEMR_SKIP_API:-0}" == "1" ]]; then
    info "NEMR_SKIP_API=1 — skipping the API round-trip"
else
    # The real proof: credentials plus network egress together. A rendered TUI
    # would demonstrate neither.
    reply=$(echo 'claude -p "Reply with exactly: NEMR_E2E_OK"' | in_container | tr -d '\r' | grep -c NEMR_E2E_OK || true)
    assert "Claude Code API round-trip succeeded" bash -c "[[ '$reply' -ge 1 ]]"
fi

# ---------------------------------------------------------------------------
# 5. Persistence across stop/start
# ---------------------------------------------------------------------------

step "persistence across stop/start (checksum, not presence)"

echo 'dd if=/dev/urandom of=/workspace/payload.bin bs=1M count=4 2>/dev/null; sha256sum /workspace/payload.bin > /workspace/CHECKSUMS; cat /workspace/CHECKSUMS' \
    | in_container >/dev/null

$NEMR stop "$PROJECT" >/dev/null
assert "task gone after stop" \
    bash -c "! ctr tasks list 2>/dev/null | grep -q 'nemr-${PROJECT}'"
assert_eq "container record survives stop" "1" "$(ctr containers list 2>/dev/null | grep -c "nemr-${PROJECT}" || true)"
assert_eq "volume stays mounted across stop" "1" "$(mount_entries_for "$PROJECT")"

$NEMR start "$PROJECT" >/dev/null
verified=$(echo 'sha256sum -c /workspace/CHECKSUMS' | in_container | tr -d '\r' | grep -c ': OK' || true)
assert_eq "checksum verifies after restart" "1" "$verified"

# ---------------------------------------------------------------------------
# 6. Reboot survival (VOL-06)
# ---------------------------------------------------------------------------

step "reboot survival — VOL-06"

# Reproduce exactly what a reboot leaves behind: container record intact in
# containerd's database, mount and loop device gone. An actual reboot is not
# needed; the resulting state is what matters, and this produces it via the same
# helper the engine uses.
$NEMR stop "$PROJECT" >/dev/null
sudo -n "$HELPER" unmount "$PROJECT" >/dev/null 2>&1
assert_eq "simulated reboot: volume unmounted" "0" "$(mount_entries_for "$PROJECT")"
assert_eq "simulated reboot: loop device detached" "0" "$(loop_devices_for "$PROJECT")"
assert "simulated reboot: container record survives, as after a real reboot" \
    bash -c "ctr containers list 2>/dev/null | grep -q 'nemr-${PROJECT}'"
assert "simulated reboot: backing file survives" \
    test -f "${VOLUME_DIR}/${PROJECT}.img"

# list must report the truth rather than the host filesystem's numbers.
listed=$($NEMR list 2>/dev/null | grep "^${PROJECT} " || true)
assert "list reports the volume as unmounted, not the host filesystem" \
    bash -c "[[ '$listed' == *unmounted* ]]"

$NEMR start "$PROJECT" >/dev/null
assert_eq "VOL-06: start remounted the volume" "1" "$(mount_entries_for "$PROJECT")"
assert_eq "VOL-06: loop device reattached" "1" "$(loop_devices_for "$PROJECT")"

recovered=$(echo 'sha256sum -c /workspace/CHECKSUMS' | in_container | tr -d '\r' | grep -c ': OK' || true)
assert_eq "VOL-06: the SAME filesystem came back, data intact" "1" "$recovered"

quota_after=$(echo 'df -B1 --output=size /workspace | tail -1' | in_container | tr -dc '0-9\n' | grep -E '^[0-9]{6,}$' | head -1 || true)
assert "VOL-06: quota back in force (${quota_after:-unknown} bytes)" \
    bash -c "[[ -n '$quota_after' && '$quota_after' -le 524288000 ]]"

# ---------------------------------------------------------------------------
# 7. List
# ---------------------------------------------------------------------------

step "list — agrees with containerd and with df"

row=$($NEMR list 2>/dev/null | grep "^${PROJECT} " || true)
assert "project appears in list" bash -c "[[ -n '$row' ]]"
assert "status reported as running" bash -c "[[ '$row' == *running* ]]"
assert "quota reported as $SIZE" bash -c "[[ '$row' == *${SIZE}* ]]"

# Cross-check the used figure against df on the host, which is the tool the
# acceptance criterion names.
df_percent=$(df --output=pcent "${MOUNT_DIR}/${PROJECT}" | tail -1 | tr -dc '0-9')
list_percent=$(sed -E 's/.*\(([0-9]+)%\).*/\1/' <<<"$row")
delta=$(( df_percent > list_percent ? df_percent - list_percent : list_percent - df_percent ))
assert "usage agrees with df (list ${list_percent}%, df ${df_percent}%)" \
    test "$delta" -le 1

# ---------------------------------------------------------------------------
# 8. Delete
# ---------------------------------------------------------------------------

step "delete — and zero residue"

$NEMR delete "$PROJECT" --yes >/dev/null
assert_eq "container record removed" "0" "$(ctr containers list 2>/dev/null | grep -c "nemr-${PROJECT}" || true)"
assert_eq "snapshot removed" "0" "$(ctr snapshots list 2>/dev/null | grep -c "nemr-${PROJECT}" || true)"
assert_eq "task removed" "0" "$(ctr tasks list 2>/dev/null | grep -c "nemr-${PROJECT}" || true)"
assert_eq "loop device released" "0" "$(loop_devices_for "$PROJECT")"
assert_eq "mount entry gone" "0" "$(mount_entries_for "$PROJECT")"
assert "backing file removed" bash -c "[[ ! -f '${VOLUME_DIR}/${PROJECT}.img' ]]"
assert "mount point removed" bash -c "[[ ! -d '${MOUNT_DIR}/${PROJECT}' ]]"
assert_eq "systemd scope gone" "0" \
    "$(systemctl --user list-units --type=scope --all 2>/dev/null | grep -c "nemr-${PROJECT}" || true)"

# The orphan class that once passed a green test: a loop device whose backing
# file was deleted is still attached but no longer matches its path. Scope this
# to THIS project's backing file — a bare `grep -ci deleted` is host-global and
# fails on any unrelated `(deleted)` loop left by something else on the machine.
assert_eq "no loop device backed by this project's (deleted) file" "0" \
    "$(losetup -a 2>/dev/null | grep -F "${VOLUME_DIR}/${PROJECT}.img" | grep -ci deleted || true)"

assert "project gone from list" \
    bash -c "! $NEMR list 2>/dev/null | grep -q '^${PROJECT} '"

# ---------------------------------------------------------------------------
# Result
# ---------------------------------------------------------------------------

printf '\n'
if [[ $FAILED -eq 0 ]]; then
    printf '\033[32mPASS\033[0m — %d steps, all assertions passed\n' "$STEP"
    exit 0
fi
printf '\033[31mFAIL\033[0m — %d assertion(s) failed across %d steps\n' "$FAILED" "$STEP"
exit 1
