#!/usr/bin/env bash
#
# NET-02 acceptance: two sessions, both binding container port 8000, both
# reachable from this host on different ports.
#
# That is the thing NET-01 made impossible — the second session's dev server
# died with EADDRINUSE, raised by the user's own program, never naming nemr —
# and it is the reason the namespace work was done, so it is what this proves.
#
# Every assertion is on HOST-observable state (curl, ss, /proc inode) or on
# output from inside a session, never on what the engine says it did.
#
#   ./scripts/netns_acceptance.sh              full: includes a live API call
#   NEMR_SKIP_API=1 ./scripts/netns_acceptance.sh   no credential needed
#
# The footer states which mode ran, because a skip-API pass is a strictly
# weaker claim and must not read as the full one.

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
REPO="$PWD"
. "$REPO/scripts/lib/proc.sh"

BLUE=$'\033[34m'; RED=$'\033[31m'; GREEN=$'\033[32m'; RESET=$'\033[0m'
STEP=0; ASSERTS=0
step() { STEP=$((STEP+1)); printf '\n%s== %d. %s%s\n' "$BLUE" "$STEP" "$1" "$RESET"; }
pass() { ASSERTS=$((ASSERTS+1)); printf '   %sok%s %s\n' "$GREEN" "$RESET" "$1"; }
fail() { printf '   %sFAIL%s %s\n' "$RED" "$RESET" "$1" >&2; exit 1; }

# Like every other script here: `ctr` below must reach the SAME containerd the
# engine uses, and inheriting it from whatever the caller happened to export is
# how a script ends up interrogating the wrong daemon and reporting on nothing.
export CONTAINERD_ADDRESS="${CONTAINERD_ADDRESS:-${XDG_RUNTIME_DIR}/containerd/containerd.sock}"

A="netns-acc-a-$$"; B="netns-acc-b-$$"
PORT_A="${NEMR_TEST_PORT_A:-18211}"; PORT_B="${NEMR_TEST_PORT_B:-18212}"
SKIP_API="${NEMR_SKIP_API:-0}"
RK_SOCK="${XDG_RUNTIME_DIR}/containerd-rootless/api.sock"

# On success, tidy up. On FAILURE, keep the sessions: the two things worth
# looking at after this script fails are the namespaces it built and what is
# left in rootlesskit's, and deleting them on the way out destroys exactly that.
# NEMR_KEEP=1 keeps them either way.
cleanup() {
    local status=$?
    set +e
    if (( status != 0 )) || [[ -n "${NEMR_KEEP:-}" ]]; then
        printf '\n   sessions %s and %s were LEFT IN PLACE for inspection.\n' "$A" "$B" >&2
        printf '   remove them with: nemr delete %s --yes && nemr delete %s --yes\n' "$A" "$B" >&2
        return
    fi
    nemr delete "$A" --yes >/dev/null 2>&1
    nemr delete "$B" --yes >/dev/null 2>&1
}
trap cleanup EXIT INT TERM

rk_child() { cat "${XDG_RUNTIME_DIR}/containerd-rootless/child_pid"; }
task_pid() { ctr -n default tasks ls 2>/dev/null | awk -v n="nemr-$1" '$1==n{print $2}'; }
netns_of() { readlink "/proc/$1/ns/net" 2>/dev/null; }
in_session() { echo "$2" | timeout 180 nemr attach "$1" 2>/dev/null | tr -d '\r'; }

# ---------------------------------------------------------------------------
step "Prerequisites"
# ---------------------------------------------------------------------------
command -v nemr >/dev/null || fail "nemr is not on PATH — ./scripts/install_engine.sh"
if ! cargo test --test regression the_installed_engine_matches_its_source --quiet >/dev/null 2>&1; then
    fail "the installed nemr/nemrd is not this source — ./scripts/install_engine.sh"
fi
pass "installed engine matches this source"

for p in "$PORT_A" "$PORT_B"; do
    if timeout 2 bash -c ">/dev/tcp/127.0.0.1/$p" 2>/dev/null; then
        fail "host port $p is already in use; a later success could be someone else's server.
        Set NEMR_TEST_PORT_A / NEMR_TEST_PORT_B to free ports."
    fi
done
pass "host ports $PORT_A and $PORT_B are free (so a later success is ours)"

# ---------------------------------------------------------------------------
step "Two sessions, each with its OWN network namespace"
# ---------------------------------------------------------------------------
for p in "$A" "$B"; do
    NEMR_NON_INTERACTIVE=1 nemr create "$p" --size 500MB >/dev/null
    nemr start "$p" >/dev/null
done
PA=$(task_pid "$A"); PB=$(task_pid "$B"); RK=$(rk_child)
NS_A=$(netns_of "$PA"); NS_B=$(netns_of "$PB"); NS_RK=$(netns_of "$RK")

# Control: all three reads must have produced something. "They differ" is
# trivially true if none of them could be read.
for pair in "A:$NS_A" "B:$NS_B" "rootlesskit:$NS_RK"; do
    case "${pair#*:}" in
        net:\[*) ;;
        *) fail "could not read ${pair%%:*}'s network namespace; this test would
        otherwise pass by failing to look" ;;
    esac
done
pass "all three namespaces are readable (control)"
[[ "$NS_A" != "$NS_B" && "$NS_A" != "$NS_RK" && "$NS_B" != "$NS_RK" ]] \
    || fail "namespaces are not distinct: A=$NS_A B=$NS_B rootlesskit=$NS_RK"
pass "A, B and rootlesskit are three distinct namespaces"

# ---------------------------------------------------------------------------
step "Both sessions bind container port 8000 — what NET-01 made impossible"
# ---------------------------------------------------------------------------
for pair in "$A SESSION-A" "$B SESSION-B"; do
    set -- $pair
    in_session "$1" "setsid nohup node -e \"require('http').createServer((q,s)=>s.end('$2')).listen(8000,'0.0.0.0')\" >/tmp/srv.log 2>&1 </dev/null & sleep 2; echo started" >/dev/null
done
# Observed from OUTSIDE, in each session's own namespace, using the host's ss:
# the base image ships no ss, and asking the server whether it is listening
# would be taking its word for it.
listening_in() {
    nsenter -t "$RK" -U -n --preserve-credentials -- \
        nsenter -t "$1" -n -- ss -tln 2>/dev/null | grep -c ':8000 ' || true
}
for pair in "$PA A" "$PB B"; do
    set -- $pair
    got=$(listening_in "$1")
    [[ "$got" == "1" ]] || fail "session $2 is not listening on 8000 in its own namespace (saw '$got')"
done
pass "both sessions hold container port 8000 simultaneously"

# ---------------------------------------------------------------------------
step "Each reachable from the host on its own port"
# ---------------------------------------------------------------------------
nemr port add "$A" "${PORT_A}:8000" >/dev/null
nemr port add "$B" "${PORT_B}:8000" >/dev/null
got_a=$(curl -fsS --max-time 5 "http://127.0.0.1:${PORT_A}/" || true)
got_b=$(curl -fsS --max-time 5 "http://127.0.0.1:${PORT_B}/" || true)
[[ "$got_a" == "SESSION-A" ]] || fail "port $PORT_A returned '${got_a:-<nothing>}', wanted SESSION-A"
[[ "$got_b" == "SESSION-B" ]] || fail "port $PORT_B returned '${got_b:-<nothing>}', wanted SESSION-B"
pass "host $PORT_A -> SESSION-A and host $PORT_B -> SESSION-B, simultaneously"
[[ "$got_a" != "$got_b" ]] || fail "both ports reached the same session — the forwards are not isolated"
pass "the two forwards reach DIFFERENT sessions (not one answering twice)"

# ---------------------------------------------------------------------------
step "Egress, with the control that proves the NAT is what provides it"
# ---------------------------------------------------------------------------
# Remove this session's MASQUERADE rule and show egress dies; restore and show
# it returns. Without this, "the internet works" might be true for a reason
# unrelated to anything the engine did.
ALLOC=$(ctr -n default containers info "nemr-$A" 2>/dev/null \
        | python3 -c "import sys,json;print(json.load(sys.stdin)['Labels'].get('nemr.netns','?'))")
[[ "$ALLOC" =~ ^[0-9]+$ ]] || fail "could not read session A's network allocation label"
CIDR="10.99.${ALLOC}.0/24"

probe_egress() { in_session "$A" 'node -e "require(\"http\").get({host:\"1.1.1.1\",port:80,timeout:4000},r=>{console.log(\"EGRESS-OK\");process.exit(0)}).on(\"error\",()=>{console.log(\"EGRESS-FAIL\");process.exit(0)}).on(\"timeout\",()=>{console.log(\"EGRESS-FAIL\");process.exit(0)})"' | tail -1; }

[[ "$(probe_egress)" == "EGRESS-OK" ]] || fail "session A cannot reach the internet"
pass "session A reaches the internet"

nsenter -t "$RK" -U -n --preserve-credentials -- \
    iptables -t nat -D POSTROUTING -s "$CIDR" -o tap0 -j MASQUERADE
[[ "$(probe_egress)" == "EGRESS-FAIL" ]] \
    || fail "egress still worked with the NAT rule removed — so the NAT is NOT what provides it,
        and this test proves nothing about the engine's networking"
pass "CONTROL: removing the MASQUERADE rule kills egress — the NAT is what provides it"

nsenter -t "$RK" -U -n --preserve-credentials -- \
    iptables -t nat -A POSTROUTING -s "$CIDR" -o tap0 -j MASQUERADE
[[ "$(probe_egress)" == "EGRESS-OK" ]] || fail "egress did not return after restoring the NAT rule"
pass "restoring it brings egress back"

# ---------------------------------------------------------------------------
step "DNS inside an isolated namespace"
# ---------------------------------------------------------------------------
in_session "$A" 'getent hosts api.anthropic.com >/dev/null && echo DNS-OK || echo DNS-FAIL' \
    | grep -q DNS-OK || fail "DNS does not resolve inside the session"
pass "api.anthropic.com resolves inside session A"

# ---------------------------------------------------------------------------
step "A real Claude Code API round-trip from an isolated namespace"
# ---------------------------------------------------------------------------
if [[ "$SKIP_API" == "1" ]]; then
    pass "skipped (NEMR_SKIP_API=1) — DNS and egress were still proven above"
else
    reply=$(in_session "$A" 'claude --permission-mode acceptEdits -p "Reply with exactly: NETNS-ACCEPT-OK" 2>&1 | tail -3')
    grep -q "NETNS-ACCEPT-OK" <<<"$reply" || {
        printf '%s\n' "$reply" | head -10 >&2
        fail "Claude Code could not complete an API round-trip from the isolated namespace"
    }
    pass "Claude Code completed a live API round-trip from inside an isolated namespace"
fi

# ---------------------------------------------------------------------------
step "Teardown releases everything"
# ---------------------------------------------------------------------------
nemr delete "$A" --yes >/dev/null
nemr delete "$B" --yes >/dev/null

# Read the state FIRST, and prove the read worked before counting anything.
# `ip ... | grep -c "^nemr" || true` yields a clean "0" when `ip` itself fails,
# so this assertion used to pass by failing to look — the same shape as an
# assertion that counts zero because it never ran. rootlesskit's namespace
# always has lo and tap0, and the nat table always prints its POSTROUTING
# policy line, so an absence of those means the instrument is broken.
links=$(nsenter -t "$RK" -U -n --preserve-credentials -- ip -brief link show 2>&1) \
    || fail "could not read rootlesskit's links, so 'no veth survived' would be unproven:
$links"
grep -q '^lo ' <<<"$links" || fail "rootlesskit's link list contains no loopback, so it is not
        a trustworthy reading; 'no veth survived' would be an artefact:
$links"
nat=$(nsenter -t "$RK" -U -n --preserve-credentials -- iptables -w 5 -t nat -S POSTROUTING 2>&1) \
    || fail "could not read the nat table, so 'no NAT rule survived' would be unproven:
$nat"
grep -q '^-P POSTROUTING' <<<"$nat" || fail "the nat table reading has no POSTROUTING policy line,
        so it is not trustworthy:
$nat"
host_sockets=$(ss -tln 2>&1) || fail "could not read the host's listening sockets:
$host_sockets"
pass "CONTROL: links, NAT table and host sockets were actually read"

# Both ends of the pair are nemr-prefixed, so a leak of either is visible here.
left_veth=$(grep -cE '^nemrc?[0-9]+[@ ]' <<<"$links" || true)
left_nat=$(grep -c '10\.99\.' <<<"$nat" || true)
[[ "$left_veth" == "0" ]] || fail "$left_veth veth interface(s) survived delete:
$(grep -E '^nemrc?[0-9]+[@ ]' <<<"$links")"
[[ "$left_nat" == "0" ]] || fail "$left_nat NAT rule(s) survived delete:
$(grep '10\.99\.' <<<"$nat")"
pass "no veth interfaces and no NAT rules survived delete"
for p in "$PORT_A" "$PORT_B"; do
    ! grep -q ":$p " <<<"$host_sockets" || fail "host port $p still bound after delete"
done
pass "both host ports released"

printf '\n%sPASS%s — %d steps, %d assertions.\n' "$GREEN" "$RESET" "$STEP" "$ASSERTS"
if [[ "$SKIP_API" == "1" ]]; then
    echo "mode: NEMR_SKIP_API=1 — egress and DNS proven, but the live API round-trip"
    echo "      was NOT exercised. Run without NEMR_SKIP_API for the full claim."
else
    echo "mode: full — including a live Claude Code API call from an isolated namespace."
fi
