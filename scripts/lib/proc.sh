#!/usr/bin/env bash
#
# Waiting for things to come up, with the diagnostic built in (F-65 class).
#
# Seven times this project has shipped a wait loop that failed silently: a
# process backgrounded, a fixed number of polls, and on failure a message that
# named the symptom and nothing else — "did not come up", log captured and never
# printed, or printed when empty so it looked like no log existed. The seventh
# was written in a script authored *after* the rule against it existed, which is
# what settled that the rule needed to be a function rather than a habit.
#
# Source it:
#
#   . "$(dirname "${BASH_SOURCE[0]}")/lib/proc.sh"
#
# Then:
#
#   wait_for_service "the sync server" "$PID" "$LOG" 15 curl -fsS http://127.0.0.1:8080/health
#   wait_for_ready   "rootless containerd" 30 test -S "$SOCK"
#   require_tcp 127.0.0.1 5433 "Postgres" "./scripts/setup_sync_test_db.sh"
#
# Every one of them returns non-zero on failure and prints why. None of them
# skips, and none of them treats "cannot tell" as success.
#
# scripts/check_wait_discipline.sh enforces that a script backgrounding a
# process uses wait_for_service rather than rolling its own loop — without that
# gate this file is only the same checklist in a new location.

# Guard against double-sourcing (callers may source it via different paths).
[[ -n "${_NEMR_PROC_SH_LOADED:-}" ]] && return 0
_NEMR_PROC_SH_LOADED=1

_PROC_GREEN=$'\033[32m'; _PROC_RED=$'\033[31m'; _PROC_RESET=$'\033[0m'
_proc_ok()   { printf '   %sok%s %s\n' "$_PROC_GREEN" "$_PROC_RESET" "$1"; }
_proc_bad()  { printf '   %sFAIL%s %s\n' "$_PROC_RED" "$_PROC_RESET" "$1" >&2; }

# How long between polls. Fixed rather than clever: the point of this file is
# that the failure is legible, not that the wait is optimal.
_PROC_INTERVAL="${NEMR_WAIT_INTERVAL:-0.5}"

# Print a captured log so it can actually be read — with a header, a per-line
# prefix so it is distinguishable from the caller's own output, and an explicit
# statement when it is empty or absent. An empty log printed as nothing looks
# like no log was captured, which sends the reader hunting the wrong thing.
_proc_dump_log() {
    printf '   --- log (%s) ---\n' "${1:-<none>}" >&2
    if [[ -z "${1:-}" ]]; then
        printf '   | (no log file was given to wait_for_service)\n' >&2
    elif [[ ! -e "$1" ]]; then
        printf '   | (no such file — nothing was captured)\n' >&2
    elif [[ -s "$1" ]]; then
        sed 's/^/   | /' "$1" >&2
    else
        printf '   | (empty — the process wrote nothing at all)\n' >&2
    fi
    printf '   --- end log ---\n' >&2
}

# Wait for a process WE started to answer a probe.
#
#   wait_for_service <name> <pid> <logfile> <timeout_s> <probe_cmd...>
#
# Returns 0 when the probe succeeds. On failure returns 1 and reports:
#   - whether the process is still alive but silent, or exited (with its status),
#   - the captured log, or the fact that there is nothing in it.
#
# It stops the moment the process dies rather than burning the rest of the
# window on something already gone: the answer is available immediately and
# waiting only delays it.
wait_for_service() {
    if [[ $# -lt 5 ]]; then
        _proc_bad "wait_for_service: usage: <name> <pid> <logfile> <timeout_s> <probe...>"
        return 2
    fi
    local name="$1" pid="$2" log="$3" timeout_s="$4"
    shift 4

    local polls=$(( timeout_s * 2 ))   # _PROC_INTERVAL is 0.5s
    (( polls < 1 )) && polls=1
    local i
    for (( i = 0; i < polls; i++ )); do
        if "$@" >/dev/null 2>&1; then
            _proc_ok "$name is answering"
            return 0
        fi
        if ! kill -0 "$pid" 2>/dev/null; then
            break
        fi
        sleep "$_PROC_INTERVAL"
    done

    # One last probe: the process may have answered and exited in the same
    # breath (a one-shot), and reporting "died" for something that worked would
    # be its own false diagnostic.
    if "$@" >/dev/null 2>&1; then
        _proc_ok "$name is answering"
        return 0
    fi

    if kill -0 "$pid" 2>/dev/null; then
        printf '   %s (pid %s) is still running but did not answer within %ss\n' \
            "$name" "$pid" "$timeout_s" >&2
    else
        local status
        if wait "$pid" 2>/dev/null; then status=0; else status=$?; fi
        if [[ "$status" == 127 ]]; then
            printf '   %s (pid %s) is gone; exit status unavailable (not a child of this shell)\n' \
                "$name" "$pid" >&2
        else
            printf '   %s (pid %s) exited with status %s before answering\n' \
                "$name" "$pid" "$status" >&2
        fi
    fi
    _proc_dump_log "$log"
    _proc_bad "$name did not come up"
    return 1
}

# Wait for something we did NOT start — a systemd unit, a container, a socket.
# Same discipline, minus the process: there is no PID to watch, so say what was
# probed and for how long rather than only that it failed.
#
#   wait_for_ready <name> <timeout_s> <probe_cmd...>
wait_for_ready() {
    if [[ $# -lt 3 ]]; then
        _proc_bad "wait_for_ready: usage: <name> <timeout_s> <probe...>"
        return 2
    fi
    local name="$1" timeout_s="$2"
    shift 2

    local polls=$(( timeout_s * 2 ))
    (( polls < 1 )) && polls=1
    local i
    for (( i = 0; i < polls; i++ )); do
        if "$@" >/dev/null 2>&1; then
            _proc_ok "$name is ready"
            return 0
        fi
        sleep "$_PROC_INTERVAL"
    done
    printf '   %s did not become ready within %ss.\n' "$name" "$timeout_s" >&2
    printf '   probed with: %s\n' "$*" >&2
    _proc_bad "$name is not ready"
    return 1
}

# Probe one TCP dependency by name, before anything that needs it runs.
#
#   require_tcp <host> <port> <what> <remedy>
#
# Exists because "the server did not come up" and "the database it needs is not
# running" are different failures with different fixes, and sharing one message
# between them cost a CI round-trip to disambiguate.
require_tcp() {
    if [[ $# -lt 4 ]]; then
        _proc_bad "require_tcp: usage: <host> <port> <what> <remedy>"
        return 2
    fi
    local host="$1" port="$2" what="$3" remedy="$4"
    if timeout 5 bash -c ">/dev/tcp/${host}/${port}" 2>/dev/null; then
        _proc_ok "$what reachable at ${host}:${port}"
        return 0
    fi
    printf '   nothing is listening on %s:%s, so %s is not reachable.\n' \
        "$host" "$port" "$what" >&2
    printf '   remedy: %s\n' "$remedy" >&2
    printf '   This is that failure, not a failure of whatever needed it.\n' >&2
    _proc_bad "$what unreachable"
    return 1
}
