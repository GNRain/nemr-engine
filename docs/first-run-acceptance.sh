#!/usr/bin/env bash
# The first-run acceptance (F-131 as extended, 2026-09-06), verbatim from the
# Product Owner: host logged in; `nemr create`, `start`, `attach`, `claude` —
# and the first thing on screen is the Claude Code prompt ready for input. No
# theme picker, no login method, no browser, no trust dialog. Proven on a
# session that has NEVER run `claude`, which is the case the 8-hour test did
# not cover.
#
#   docs/first-run-acceptance.sh            # creates firstrun-<pid>, proves, deletes it
#   docs/first-run-acceptance.sh <project>  # proves an existing, never-used project; does not delete it
#
# Drives an interactive `claude` inside the session through `nemr attach`
# under a pty and reads the screen. Everything captured is kept (F-132): the
# stripped screen is printed on failure, and the raw capture stays in /tmp.
set -u
own=0; project="${1:-}"
if [ -z "$project" ]; then project="firstrun-$$"; own=1; fi
raw="/tmp/nemr-firstrun-$project.raw"; txt="/tmp/nemr-firstrun-$project.txt"

echo "=== first-run acceptance for $project at $(date -u +%FT%TZ) ==="
python3 - <<'PY' || exit 1
import json,os,time
p=os.path.expanduser('~/.claude/.credentials.json')
if not os.path.exists(p): print("no host credential; log in on the host first"); raise SystemExit(1)
o=json.load(open(p))['claudeAiOauth']
print("host credential: access %s (%+.1f h), refresh to %s" % ("present" if o.get('accessToken') else "BLANK", (o['expiresAt']/1000-time.time())/3600, time.strftime('%F', time.gmtime(o.get('refreshTokenExpiresAt',0)/1000))))
PY
if [ "$own" = 1 ]; then
    NEMR_NON_INTERACTIVE=1 nemr create "$project" --size 500MB >/dev/null || exit 1
    nemr start "$project" >/dev/null || exit 1
fi
# A never-used session: no .claude.json written by Claude Code yet. The seed
# nemr wrote at start is expected; anything Claude Code wrote means it ran.
seen=$(echo 'node -e "const c=require(\"/root/.claude.json\");console.log(c.machineID?\"CLAUDE-RAN\":\"fresh\")" 2>/dev/null || echo fresh' | nemr attach "$project" 2>&1 | tr -d '\r' | grep -E '^(CLAUDE-RAN|fresh)$' | tail -1)
[ "$seen" = "fresh" ] || { echo "FAIL — $project has already run claude (machineID present); this acceptance needs a never-used session"; exit 1; }

# claude, then a scripted wait, then Ctrl-C twice and exit — the screen is the evidence.
( sleep 3; printf 'claude\r'; sleep 16; printf '\003'; sleep 0.3; printf '\003'; sleep 1.5; printf 'exit\r'; sleep 1 ) \
  | timeout 90 script -qfec "nemr attach $project" /dev/null > "$raw" 2>&1
sed -r 's/\x1b\[[0-9;?]*[a-zA-Z]//g; s/\x1b\][^\x07]*\x07//g; s/\r//g' "$raw" > "$txt"
flat=$(tr -d ' ' < "$txt")
count() { grep -o "$1" <<<"$flat" | wc -l; }
theme=$(count 'Choosethetext'); login=$(count 'Selectloginmethod'); trust=$(count 'Isthisaproject'); ready=$(( $(count 'forshortcuts') + $(count 'Try"') ))
echo "screen: theme picker=$theme  login method=$login  trust dialog=$trust  ready prompt=$ready"
if [ "$own" = 1 ]; then
    # Cleanup verifies before destroying, and never touches a protected subject.
    case "$project" in htmltest) echo "refusing to delete a protected subject";; *) nemr list 2>/dev/null | grep -q "^$project " && nemr delete "$project" --yes >/dev/null 2>&1;; esac
fi
if [ "$theme" = 0 ] && [ "$login" = 0 ] && [ "$trust" = 0 ] && [ "$ready" -gt 0 ]; then
    echo "PASS — claude opened ready for input on a never-used session; no theme picker, no login method, no trust dialog"
else
    echo "FAIL — the screen (stripped; raw capture kept at $raw):"; grep -v '^\s*$' "$txt" | tail -30 | sed 's/^/   | /'; exit 1
fi
