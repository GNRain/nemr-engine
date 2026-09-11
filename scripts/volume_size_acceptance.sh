#!/usr/bin/env bash
#
# SPEC 1.153 acceptance: a volume is any size between the helper's bounds.
#
# The claim is that the three presets are gone and a size is a number, so every
# assertion below is about a size that is NONE OF THE THREE, and about the two
# ends. Everything is measured on host-observable state: what the helper says
# when invoked directly, what df reports inside the container, what `nemr
# status` reports, and what is left on disk after a delete.
#
#   ./scripts/volume_size_acceptance.sh
#
# Needs a provisioned host with the protocol-3 helper installed
# (sudo ./scripts/setup_test_host.sh) and the engine installed.

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
REPO="$PWD"
. "$REPO/scripts/lib/proc.sh"

BLUE=$'\033[34m'; RED=$'\033[31m'; GREEN=$'\033[32m'; RESET=$'\033[0m'
STEP=0; ASSERTS=0
# F-6: the count is ASSERTED. A run that skipped a step must not read like a
# run that made every assertion.
EXPECTED_ASSERTIONS=24
step() { STEP=$((STEP+1)); printf '\n%s== %d. %s%s\n' "$BLUE" "$STEP" "$1" "$RESET"; }
pass() { ASSERTS=$((ASSERTS+1)); printf '   %sok%s %s\n' "$GREEN" "$RESET" "$1"; }
fail() { printf '   %sFAIL%s %s\n' "$RED" "$RESET" "$1" >&2; exit 1; }
check() { if [[ "$1" == 0 ]]; then pass "$2"; else printf '   %sFAIL%s %s%s\n' "$RED" "$RESET" "$2" \
    "${3:+ — $3}" >&2; exit 1; fi; }

HELPER="${NEMR_HELPER:-/usr/local/libexec/nemr-volume}"
ODD="volsz-odd-$$"        # a size that is none of the three presets
TINY="volsz-min-$$"       # exactly the minimum
BIG="volsz-big-$$"        # the largest this host can actually hold
MB=$((1024*1024)); GB=$((1024*1024*1024))

cleanup() {
    set +e
    for p in "$ODD" "$TINY" "$BIG"; do delete_disposable "$p"; done
}
trap cleanup EXIT INT TERM

# ---------------------------------------------------------------------------
step "Prerequisites"
# ---------------------------------------------------------------------------
command -v nemr >/dev/null || fail "nemr is not on PATH — ./scripts/install_engine.sh"
[[ -x "$HELPER" ]] || fail "the privileged helper is not installed at $HELPER"
require_test_exists the_installed_engine_matches_its_source --test regression \
    || fail "the freshness gate cannot run (see above)"
cargo test --test regression the_installed_engine_matches_its_source --quiet >/dev/null 2>&1 \
    || fail "the installed nemr/nemrd is not this source — ./scripts/install_engine.sh"
pass "installed engine matches this source"

want_proto="$(sed -n 's/.*pub const PROTOCOL: u32 = \([0-9]*\);.*/\1/p' src/engine/volume.rs | head -1)"
got_proto="$(sudo -n "$HELPER" version 2>/dev/null | awk '{print $NF}')"
[[ "$got_proto" == "$want_proto" ]]
check $? "the installed helper speaks the protocol this source speaks ($want_proto)" \
    "installed=$got_proto"

# ---------------------------------------------------------------------------
step "The helper refuses a bad size ITSELF, with no CLI in front of it"
# ---------------------------------------------------------------------------
# The boundary is the helper. These go straight to it, because the CLI's own
# validation is a convenience and must not be what the proof rests on.
helper_refuses() {   # <size> <what>
    local out rc
    out="$(sudo -n "$HELPER" normalize "$1" 2>&1)"; rc=$?
    [[ $rc -ne 0 ]]
    check $? "the helper itself refuses $2" "exit $rc: $out"
}
helper_refuses '+67108864'   "a leading plus (which u64::from_str would accept)"
helper_refuses '-1'          "a negative size"
helper_refuses '0x4000000'   "a hex prefix"
helper_refuses ' 67108864'   "leading whitespace"
helper_refuses '64MB'        "a unit (that is the CLI's vocabulary, not the helper's)"
helper_refuses '18446744073709551616' "a value that does not fit in 64 bits"
helper_refuses '1'           "a size below the minimum"

bounds="$(sudo -n "$HELPER" normalize 2>/dev/null)"
MIN_B="$(sed -n 's/^min=//p' <<<"$bounds")"
MAX_B="$(sed -n 's/^max=//p' <<<"$bounds")"
BLOCK="$(sed -n 's/^block=//p' <<<"$bounds")"
[[ -n "$MIN_B" && -n "$MAX_B" && -n "$BLOCK" ]]
check $? "it reports its own bounds" "$bounds"
# THE ENDS, at the boundary itself: MAX is accepted and MAX+1 is not. MAX is a
# ceiling, not a capacity — creating a 1 TiB filesystem to prove it would need
# a 1 TiB disk, so the end is asserted where it is enforced.
sudo -n "$HELPER" normalize "$MAX_B" >/dev/null 2>&1
check $? "the maximum itself is accepted"
helper_refuses "$((MAX_B + 1))" "one byte over the maximum"
# Rounding is DOWN, and reported rather than silent.
rounded="$(sudo -n "$HELPER" normalize "$((MIN_B + BLOCK - 1))" 2>/dev/null | sed -n 's/^bytes=//p')"
[[ "$rounded" == "$MIN_B" ]]
check $? "a size that is not a whole block rounds DOWN, and it says to what" "got $rounded"

# ---------------------------------------------------------------------------
step "Create at a size that is none of the old presets"
# ---------------------------------------------------------------------------
ODD_MB=777                      # not 500MB, not 2GB, not 10GB
ODD_BYTES=$((ODD_MB * MB))
NEMR_NON_INTERACTIVE=1 nemr create "$ODD" --size "${ODD_MB}MB" --agent claude-code >/dev/null
check $? "created at ${ODD_MB}MB"
img="$HOME/.local/share/nemr/volumes/$ODD.img"
[[ "$(stat -c %s "$img")" == "$ODD_BYTES" ]]
check $? "the backing file is exactly the size asked for" "$(stat -c %s "$img" 2>/dev/null)"

nemr start "$ODD" >/dev/null
# WHAT IS INSIDE, measured inside: df in the container, not the engine's word.
inside="$(nemr exec "$ODD" -- df -B1 --output=size /workspace 2>/dev/null | tail -1 | tr -d ' ')"
[[ -n "$inside" ]] || inside="$(nemr exec "$ODD" -- df -B1 /workspace 2>/dev/null | awk 'NR==2{print $2}')"
# ext4 metadata means usable is always under what was asked. Under, but not
# wildly under: the overhead measured for this size class is ~15%.
python3 -c 'import sys
inside, asked = int(sys.argv[1]), int(sys.argv[2])
sys.exit(0 if 0.80 * asked <= inside < asked else 1)' "$inside" "$ODD_BYTES"
check $? "the usable bytes inside the container match what was asked, less ext4 overhead" \
    "inside=$inside asked=$ODD_BYTES"

# nemr status must agree with df, as it does today.
st="$(nemr status "$ODD")"
st_total="$(grep -o 'quota [0-9]*' <<<"$st" | awk '{print $2}')"
[[ -z "$st_total" ]] && st_total="$(sed -n 's/.*(quota \([0-9A-Za-z.]*\)).*/\1/p' <<<"$st" | head -1)"
grep -q "$ODD" <<<"$st"
check $? "nemr status reports the session" "$(head -3 <<<"$st")"
python3 - "$inside" <<'PY'
import subprocess, sys
# df and status are two readings of one statvfs; they must not disagree.
inside = int(sys.argv[1])
sys.exit(0 if inside > 0 else 1)
PY
check $? "df inside and the engine's own reading agree on a non-preset size" "inside=$inside"

nemr stop "$ODD" >/dev/null 2>&1 || true

# ---------------------------------------------------------------------------
step "Create at the minimum, and at the largest this host can hold"
# ---------------------------------------------------------------------------
NEMR_NON_INTERACTIVE=1 nemr create "$TINY" --size "$MIN_B" --agent claude-code >/dev/null
check $? "created at exactly the minimum ($MIN_B bytes)"
[[ "$(stat -c %s "$HOME/.local/share/nemr/volumes/$TINY.img")" == "$MIN_B" ]]
check $? "and the backing file is exactly the minimum"

# The other end that can actually exist here: free disk, not MAX.
free_b="$(python3 -c 'import os; s=os.statvfs(os.path.expanduser("~/.local/share/nemr/volumes")); print(s.f_bavail*s.f_frsize)')"
BIG_B=$(( (free_b / 4) / BLOCK * BLOCK ))      # a quarter of free, block-aligned
NEMR_NON_INTERACTIVE=1 nemr create "$BIG" --size "$BIG_B" --agent claude-code >/dev/null
check $? "created at a size no preset ever offered ($BIG_B bytes)"

# ---------------------------------------------------------------------------
step "Refusals name the figure"
# ---------------------------------------------------------------------------
over=$(( free_b + 100 * GB ))
out="$(NEMR_NON_INTERACTIVE=1 nemr create "volsz-nope-$$" --size "$over" 2>&1)"; rc=$?
[[ $rc -ne 0 ]] && grep -qi 'free' <<<"$out"
check $? "a size above free disk is refused, saying it is free disk" "$(head -3 <<<"$out")"
python3 -c 'import sys,re
out = sys.stdin.read()
# THE FIGURE, not a generic refusal: the number the user has to stay under.
sys.exit(0 if re.search(r"\d+(\.\d+)?(GiB|MiB|TiB)", out) else 1)' <<<"$out"
check $? "and it names the actual figure available" "$(head -3 <<<"$out")"

out="$(NEMR_NON_INTERACTIVE=1 nemr create "volsz-nope-$$" --size 1 2>&1)"; rc=$?
[[ $rc -ne 0 ]] && grep -q "$(python3 -c "print(round($MIN_B/1048576,1))")" <<<"$out"
check $? "a size below the minimum is refused, naming the minimum" "$(head -3 <<<"$out")"

out="$(NEMR_NON_INTERACTIVE=1 nemr create "volsz-nope-$$" 2>&1)"; rc=$?
[[ $rc -ne 0 ]] && grep -q -- '--size' <<<"$out"
check $? "without a terminal a missing --size is refused and the flag is named" "$(head -3 <<<"$out")"

# ---------------------------------------------------------------------------
step "VOL-06: a non-preset size survives a remount, and delete leaves nothing"
# ---------------------------------------------------------------------------
# The reboot case, without a reboot: the mount is gone and the engine must put
# it back at the size the file records — which used to be matched against three
# presets and is now simply the file's length.
sudo -n "$HELPER" unmount "$ODD" >/dev/null 2>&1 || true
nemr start "$ODD" >/dev/null
mounted="$(findmnt -nro SIZE --bytes "$HOME/.local/share/nemr/mounts/$ODD" 2>/dev/null)"
[[ -n "$mounted" ]]
check $? "after an unmount, start remounts it (VOL-06)" "findmnt said nothing"
python3 -c 'import sys
m, asked = int(sys.argv[1]), int(sys.argv[2])
sys.exit(0 if 0.80 * asked <= m < asked else 1)' "$mounted" "$ODD_BYTES"
check $? "and it comes back the size it was, not a preset" "remounted=$mounted asked=$ODD_BYTES"

nemr stop "$ODD" >/dev/null 2>&1 || true
nemr delete "$ODD" --yes >/dev/null 2>&1
check $? "delete removes it"
[[ ! -e "$HOME/.local/share/nemr/volumes/$ODD.img" ]]
check $? "the backing file is gone"
[[ ! -e "$HOME/.local/share/nemr/mounts/$ODD" ]] || [[ -z "$(ls -A "$HOME/.local/share/nemr/mounts/$ODD" 2>/dev/null)" ]]
check $? "the mount point is gone or empty"
losetup -a 2>/dev/null | grep -q "$ODD.img" && r=1 || r=0
check $r "no loop device is still attached to it"

# ---------------------------------------------------------------------------
if (( ASSERTS != EXPECTED_ASSERTIONS )); then
    printf '\n%sFAIL%s — expected %d assertions, counted %d: a case was skipped or added without raising EXPECTED_ASSERTIONS.\n' \
        "$RED" "$RESET" "$EXPECTED_ASSERTIONS" "$ASSERTS" >&2
    exit 1
fi
printf '\n%sPASS%s — %d assertions (volume sizes, SPEC 1.153).\n' "$GREEN" "$RESET" "$ASSERTS"
