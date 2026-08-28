#!/usr/bin/env bash
#
# F-95 enforcement: a script that backgrounds a process must wait for it with
# wait_for_service, not with a hand-rolled loop.
#
# Seven times this project shipped a wait loop whose failure said nothing
# useful — the seventh in a script written AFTER the rule against it existed.
# That is what settled it: the rule competes with the moment of writing and
# loses, so it has to be a gate. scripts/lib/proc.sh is the helper; this is what
# makes using it non-optional.
#
# The rule: if a script captures a backgrounded PID (`$!`), it must source
# lib/proc.sh and call wait_for_service. Capturing `$!` is the proxy for "this
# script owns a process whose readiness someone will wait on", and it is the
# shape every one of the seven had.
#
#   ./scripts/check_wait_discipline.sh
#
# Exit 0 = the discipline holds, 1 = a script rolled its own.

set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

GREEN=$'\033[32m'; RED=$'\033[31m'; RESET=$'\033[0m'
VIOLATIONS=0
report() { printf '%sVIOLATION%s %s\n' "$RED" "$RESET" "$*"; VIOLATIONS=$((VIOLATIONS + 1)); }
ok()     { printf '%sok%s        %s\n' "$GREEN" "$RESET" "$*"; }

# Does one script obey the rule? Prints nothing; returns 0 (fine) or 1 (rolled
# its own). Factored out so the control below runs the SAME logic the real
# check runs — a control that re-implements the rule tests the re-implementation.
wait_discipline_holds() {
    local file="$1"
    grep -qE '(^|[^a-zA-Z0-9_])\$!' "$file" 2>/dev/null || return 0   # no background pid: not our business
    grep -q 'wait_for_service' "$file" 2>/dev/null && return 0
    return 1
}

mapfile -t candidates < <(find scripts -maxdepth 2 -name '*.sh' -type f 2>/dev/null | sort)
offenders=()
backgrounding=0
for f in "${candidates[@]}"; do
    [[ "$f" == "scripts/check_wait_discipline.sh" ]] && continue   # this file names the pattern
    [[ "$f" == "scripts/lib/proc.sh" ]] && continue                # the helper itself
    [[ "$f" == "scripts/test_proc_lib.sh" ]] && continue           # tests the helper directly
    if grep -qE '(^|[^a-zA-Z0-9_])\$!' "$f" 2>/dev/null; then
        backgrounding=$((backgrounding + 1))
        wait_discipline_holds "$f" || offenders+=("$f")
    fi
done

if (( ${#offenders[@]} > 0 )); then
    report "a script backgrounds a process without wait_for_service:"
    for f in "${offenders[@]}"; do
        printf '          %s\n' "$f"
        grep -nE '(^|[^a-zA-Z0-9_])\$!' "$f" | sed 's/^/            /'
    done
    printf '          %s\n' "Use scripts/lib/proc.sh:"
    printf '          %s\n' "  . \"\$(dirname \"\${BASH_SOURCE[0]}\")/lib/proc.sh\""
    printf '          %s\n' "  wait_for_service <name> \"\$PID\" \"\$LOG\" <timeout_s> <probe...>"
else
    ok "every script that backgrounds a process waits with wait_for_service ($backgrounding scanned)"
fi

# ---------------------------------------------------------------------------
# Control — self-contained, and it exercises the real predicate
# ---------------------------------------------------------------------------
# Without this, a change that made `wait_discipline_holds` always-true would
# leave the check green while enforcing nothing — the exact green-over-nothing
# shape that motivated the helper in the first place.
probe=$(mktemp -d)
trap 'rm -rf "$probe"' EXIT

cat > "$probe/offender.sh" <<'PROBE'
#!/usr/bin/env bash
server & SERVER_PID=$!
for _ in $(seq 1 30); do curl -fsS localhost:8080 && break; sleep 0.5; done
PROBE

cat > "$probe/compliant.sh" <<'PROBE'
#!/usr/bin/env bash
. "$(dirname "${BASH_SOURCE[0]}")/lib/proc.sh"
server & SERVER_PID=$!
wait_for_service "server" "$SERVER_PID" "$LOG" 15 curl -fsS localhost:8080
PROBE

cat > "$probe/innocent.sh" <<'PROBE'
#!/usr/bin/env bash
echo "this script starts nothing in the background"
PROBE

if wait_discipline_holds "$probe/offender.sh"; then
    report "control failed: a hand-rolled wait loop was NOT flagged. The check \
matches nothing, so its 'ok' above is meaningless."
elif ! wait_discipline_holds "$probe/compliant.sh"; then
    report "control failed: a script using wait_for_service WAS flagged. The \
check is too broad and would reject the very pattern it exists to require."
elif ! wait_discipline_holds "$probe/innocent.sh"; then
    report "control failed: a script that backgrounds nothing was flagged."
else
    ok "control: a hand-rolled loop is caught, a compliant script and a script \
that backgrounds nothing are not — the check discriminates"
fi

printf '\n'
if [[ $VIOLATIONS -eq 0 ]]; then
    printf '%sPASS%s — wait discipline holds: no script rolls its own readiness loop.\n' "$GREEN" "$RESET"
    exit 0
fi
printf '%sFAIL%s — %d violation(s). A wait that cannot explain its own failure is the F-65 class.\n' \
    "$RED" "$RESET" "$VIOLATIONS"
exit 1
