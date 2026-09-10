#!/usr/bin/env bash
# The credential acceptance (D-02 (f), E6), re-runnable on any host with the
# engine installed: is a RUNNING session's Claude Code still able to answer
# after the host's access token expired and the host refreshed meanwhile?
#
#   docs/credential-acceptance.sh <project>      # the project must be running
#
# Reads, and one `claude -p` inside the session. Everything the session says
# is kept — stderr included, exit code reported — because the first version of
# this check discarded stderr and left an empty answer unexplained (F-132, the
# F-65 class: a check that erases the evidence of its own failure).
set -u
project="${1:?usage: $0 <project>}"
# The engine's own login (F-14), not the host's ~/.claude — which is a
# different file that nemr neither writes nor reads.
cred="$HOME/.local/share/nemr/host-credential/.credentials.json"
log="${XDG_STATE_HOME:-$HOME/.local/state}/nemr/nemrd.log"

echo "=== credential acceptance for $project at $(date -u +%FT%TZ) ==="
python3 - "$cred" <<'PY'
import json,sys,time
o=json.load(open(sys.argv[1])).get('claudeAiOauth', {})
if '_nemr_placeholder' in o:
    print("this machine has no nemr login yet (placeholder) — run /login inside a session"); raise SystemExit(1)
print("host: access token %s (%+.1f h), refresh token to %s" % (
    "BLANK" if not o.get('accessToken') else "present",
    (o['expiresAt']/1000-time.time())/3600,
    time.strftime('%F', time.gmtime(o.get('refreshTokenExpiresAt',0)/1000))))
PY
nemr status "$project" | grep -E 'credential:|last rewrite' || true
echo "--- the watcher's record of rewrites (daemon log):"
grep -E 'credential (replaced|rewritten|removed)|re-bound' "$log" 2>/dev/null | tail -4 || echo "   (no daemon log at $log)"

echo "--- claude -p inside the session (stdout, then stderr, then the exit code — nothing discarded):"
out=$(printf '%s\n' 'claude -p --output-format json "Reply with the single word OK."; echo "NEMR_EXIT=$?"' | nemr attach "$project" 2>&1 | tr -d '\r')
printf '%s\n' "$out" | sed 's/^/   | /' | tail -25
exit_line=$(printf '%s\n' "$out" | grep -o 'NEMR_EXIT=[0-9]*' | tail -1)
result=$(printf '%s\n' "$out" | grep -o '"result":"[^"]*"' | tail -1)
echo "--- verdict: ${result:-no result field seen}, ${exit_line:-exit code not seen}"
case "$result" in *'"OK"'*) echo "PASS — the session answered past the window";; *) echo "FAIL — read the output above; the evidence is all there"; exit 1;; esac
