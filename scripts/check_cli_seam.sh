#!/usr/bin/env bash
#
# The `nemr` CLI must have NO direct path into containerd (E-09).
#
# Two processes independently mutating containerd state and mount records is the
# divergence class WP A spent nine commits eliminating. The daemon makes
# single-writer structural: the CLI is a gRPC client, and the daemon is the only
# writer. This check enforces that the CLI never reacquires a containerd path —
# by construction, not by convention.
#
#   ./scripts/check_cli_seam.sh

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

CLI="src/bin/nemr.rs"
DAEMON="src/daemon/mod.rs"
RED=$'\033[31m'; GREEN=$'\033[32m'; RESET=$'\033[0m'

# The containerd path: opening a client, or calling the engine's project ops
# that drive containerd. The CLI must reach these ONLY through the daemon.
PATTERN='ContainerdClient|engine::project::|containerd::client|containerd::containers'

cli_hits="$(grep -nE "$PATTERN" "$CLI" | grep -vE '^\s*[0-9]+:\s*//' || true)"

# CONTROL: the DAEMON must contain the containerd path — it is the single
# writer. If the pattern matches nothing there, the pattern is wrong and a clean
# CLI result would be meaningless (the F-80 lesson: a control that would fail if
# the check were testing nothing).
daemon_hits="$(grep -cE 'engine::project::|ContainerdClient' "$DAEMON" || true)"
if [[ "$daemon_hits" -eq 0 ]]; then
    printf '%sFAIL%s — control: the daemon (%s) contains no containerd path.\n' \
        "$RED" "$RESET" "$DAEMON" >&2
    printf '       The pattern matches nothing even where it SHOULD, so a clean CLI result\n' >&2
    printf '       would prove nothing. Fix the pattern.\n' >&2
    exit 1
fi
printf '%sok%s        control: the daemon holds the containerd path (%d references)\n' \
    "$GREEN" "$RESET" "$daemon_hits"

if [[ -n "$cli_hits" ]]; then
    printf '\n%sFAIL%s — the CLI (%s) has a direct containerd path:\n' "$RED" "$RESET" "$CLI" >&2
    printf '%s\n' "$cli_hits" | sed 's/^/          /' >&2
    printf '\n       Every command must go through the daemon (nemr_engine::daemon::client),\n' >&2
    printf '       so containerd has a single writer (E-09). Route this through an RPC.\n' >&2
    exit 1
fi

printf '\n%sPASS%s — the CLI has no direct containerd path; the daemon is the single writer.\n' \
    "$GREEN" "$RESET"
