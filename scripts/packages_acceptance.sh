#!/usr/bin/env bash
#
# F-118 acceptance: a session's tools travel with it.
#
# The flow: install jq in a session, export, import onto CLEAN state, prove jq
# is genuinely ABSENT (presence of a list is not presence of a package, and the
# import must be pristine — proven, not assumed), provision, prove jq is present
# AND WORKING. Then determinism: two exports of the unchanged project must be
# byte-identical with the list aboard.
#
# Needs: a provisioned host, the engine installed and current, and network in
# sessions (provision installs from the archive).

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
REPO="$PWD"
. "$REPO/scripts/lib/proc.sh"

BLUE=$'\033[34m'; RED=$'\033[31m'; GREEN=$'\033[32m'; RESET=$'\033[0m'
STEP=0; ASSERTS=0
# F-6: the count is ASSERTED, not printed. The gate audit (2026-09-09) found
# six of this project's eight acceptance scripts exiting 0 over a tally nobody
# checked — a run that skipped a step read exactly like a run that made every
# assertion. Raise this number when a step is added; a run that counts anything
# else fails.
EXPECTED_ASSERTIONS=9
step() { STEP=$((STEP+1)); printf '\n%s== %d. %s%s\n' "$BLUE" "$STEP" "$1" "$RESET"; }
pass() { ASSERTS=$((ASSERTS+1)); printf '   %sok%s %s\n' "$GREEN" "$RESET" "$1"; }
fail() { printf '   %sFAIL%s %s\n' "$RED" "$RESET" "$1" >&2; exit 1; }

export CONTAINERD_ADDRESS="${CONTAINERD_ADDRESS:-${XDG_RUNTIME_DIR}/containerd/containerd.sock}"
SRC="pkg-src-$$"; DST="pkg-dst-$$"
WORK="$(mktemp -d)"
cleanup() {
    local status=$?
    set +e
    if (( status != 0 )) || [[ -n "${NEMR_KEEP:-}" ]]; then
        printf '\n   projects %s and %s LEFT IN PLACE for inspection.\n' "$SRC" "$DST" >&2
    else
        delete_disposable "$SRC"
        delete_disposable "$DST"
    fi
    rm -rf "$WORK"
}
trap cleanup EXIT INT TERM

in_s(){ echo "$2" | timeout 300 nemr attach "$1" 2>/dev/null | tr -d '\r'; }

# ---------------------------------------------------------------------------
step "Prerequisites (freshness gate: the binaries are this source)"
# ---------------------------------------------------------------------------
command -v nemr >/dev/null || fail "nemr is not on PATH — ./scripts/install_engine.sh"
# A filter that matches nothing exits 0, so check it names a real test BEFORE
# trusting its result — otherwise a rename disarms this gate in silence.
require_test_exists the_installed_engine_matches_its_source --test regression \
    || fail "the freshness gate cannot run (see above)"
if ! cargo test --test regression the_installed_engine_matches_its_source --quiet >/dev/null 2>&1; then
    fail "the installed nemr/nemrd is not this source — ./scripts/install_engine.sh"
fi
pass "installed engine matches this source"

# ---------------------------------------------------------------------------
step "Install jq in a session — the declaration is made by USING apt, not by an engine command"
# ---------------------------------------------------------------------------
NEMR_NON_INTERACTIVE=1 nemr create "$SRC" --size 500MB >/dev/null
nemr start "$SRC" >/dev/null
got=$(in_s "$SRC" 'command -v jq >/dev/null && echo PRESENT || echo ABSENT' | tail -1)
[[ "$got" == "ABSENT" ]] || fail "CONTROL: jq must be absent before the install (got '$got')"
pass "CONTROL: jq absent in the fresh source session"
got=$(in_s "$SRC" 'apt-get update >/dev/null 2>&1 && apt-get install -y jq >/dev/null 2>&1; command -v jq >/dev/null && jq --version || echo INSTALL-FAILED' | tail -1)
[[ "$got" == jq-* ]] || fail "installing jq in the source session failed (got '$got')"
pass "jq installed by the session's own apt ($got)"
nemr stop "$SRC" >/dev/null

# ---------------------------------------------------------------------------
step "Export detects the declaration"
# ---------------------------------------------------------------------------
B1="$WORK/one.nemr"
nemr export "$SRC" -o "$B1" >/dev/null
python3 - "$B1" <<'PY' || fail "the bundle does not carry the declaration"
import tarfile, json, sys
with tarfile.open(sys.argv[1]) as t:
    m = json.load(t.extractfile("manifest.json"))
    names = [x["path"] for x in m["members"]]
    assert ".nemr-state/packages.json" in names, f"not in bundle members: {names}"
PY
pass "the bundle carries .nemr-state/packages.json"

# ---------------------------------------------------------------------------
step "Determinism: the package list adds NO nondeterminism to export"
# ---------------------------------------------------------------------------
# The format's documented property is "identical but for created_at" — the
# manifest embeds a wall-clock stamp by design, and the existing guard blanks
# it before comparing. So that is what is asserted here: content and members
# identical, created_at the ONLY permitted difference. The first version of
# this step compared raw bytes, asserted a stronger property than the format
# promises, and fired whenever two exports straddled a second boundary — with
# a message naming the package list as the cause it had not established.
B2="$WORK/two.nemr"
nemr export "$SRC" -o "$B2" >/dev/null
python3 - "$B1" "$B2" <<'PY2' || fail "two exports of unchanged content differ beyond created_at —
        something (the package list is the new suspect) leaked nondeterminism"
import tarfile, json, sys, hashlib
def load(p):
    with tarfile.open(p) as t:
        manifest = json.load(t.extractfile("manifest.json"))
        chunks = {}
        for name in t.getnames():
            if name != "manifest.json":
                chunks[name] = hashlib.sha256(t.extractfile(name).read()).hexdigest()
        return manifest, chunks
m1, c1 = load(sys.argv[1]); m2, c2 = load(sys.argv[2])
assert c1 == c2, f"chunk payloads differ: {set(c1.items()) ^ set(c2.items())}"
m1["created_at"] = m2["created_at"] = ""
assert m1 == m2, "manifests differ beyond created_at"
PY2
pass "identical but for created_at — the documented property, with the list aboard"

# ---------------------------------------------------------------------------
step "Import onto clean state, and PROVE the import is pristine"
# ---------------------------------------------------------------------------
import_out=$(nemr import "$DST" "$B1" 2>&1)
grep -q "nemr provision $DST" <<<"$import_out" \
    || fail "import did not suggest provisioning. Output:
$import_out"
pass "import suggests provision (and does not run it)"
nemr start "$DST" >/dev/null
got=$(in_s "$DST" 'command -v jq >/dev/null && echo PRESENT || echo ABSENT' | tail -1)
[[ "$got" == "ABSENT" ]] || fail "CONTROL FAILED: jq is already present after import (got '$got')
        — the import is not pristine, so provisioning below would prove nothing"
pass "CONTROL: jq genuinely ABSENT after import, before provision (presence of a list is not presence of a package)"

# ---------------------------------------------------------------------------
step "Provision installs the declared packages"
# ---------------------------------------------------------------------------
prov_out=$(nemr provision "$DST" 2>&1) || fail "provision failed:
$prov_out"
grep -q "provisioned 1: jq" <<<"$prov_out" || fail "provision did not report jq. Output:
$prov_out"
pass "provision reports: $(grep provisioned <<<"$prov_out")"

# ---------------------------------------------------------------------------
step "The tool is present AND WORKS — the whole point"
# ---------------------------------------------------------------------------
got=$(in_s "$DST" 'echo "{\"a\":42}" | jq -r .a 2>/dev/null || echo BROKEN' | tail -1)
[[ "$got" == "42" ]] || fail "jq is installed but does not work (got '$got')"
pass "jq executes and produces correct output in the imported session"
nemr stop "$DST" >/dev/null

if [[ "$ASSERTS" -ne "$EXPECTED_ASSERTIONS" ]]; then
    printf '\n%sFAIL%s — %d assertions, %d expected: a step was skipped, or one was\n' \
        "$RED" "$RESET" "$ASSERTS" "$EXPECTED_ASSERTIONS"
    printf '       added without raising EXPECTED_ASSERTIONS. Zero failures over too\n'
    printf '       few assertions is not a pass (F-6).\n'
    exit 1
fi
printf '\n%sPASS%s — %d steps, %d assertions, all %d expected.\n' \
    "$GREEN" "$RESET" "$STEP" "$ASSERTS" "$EXPECTED_ASSERTIONS"
