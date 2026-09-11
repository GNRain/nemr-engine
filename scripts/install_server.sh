#!/usr/bin/env bash
#
# nemr — install a sync server on this machine (D-14).
#
#   ./scripts/install_server.sh          # shows the plan, asks once, installs
#   ./scripts/install_server.sh --yes    # same, without the question
#
# WHO THIS IS FOR. Someone running their OWN server. Using nemr does not need
# it: `scripts/install.sh` puts the engine and the client on the machine you
# work on, and the client points at a server. This is the other end.
#
# WHAT IT IS NOT. Not a production deployment. It gives you one server on one
# host: Postgres in a rootless container, bundles in a directory, the process
# in the foreground. No TLS, no reverse proxy, no backups, no unit. Put it
# behind something that terminates TLS before anyone but you uses it.
#
# The same rules as install.sh (D-14): the whole plan first, one question,
# `--yes` to skip it, refuse rather than proceed silently with no terminal, and
# safe to run twice.

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
REPO="$PWD"

. scripts/lib/cat.sh
. scripts/lib/steps.sh
# shellcheck source=lib/proc.sh
. scripts/lib/proc.sh

YES=0
usage() {
    cat <<'USAGE'
nemr install_server — Postgres, a pepper, a bundle store and nemr-sync, on this host.

  ./scripts/install_server.sh          show the plan, ask once, install
  ./scripts/install_server.sh --yes    accept the plan without the question
  ./scripts/install_server.sh --quiet  no animation; step lines only

For the machine you WORK on, you want scripts/install.sh instead (D-14).
USAGE
}
while (($#)); do
    case "$1" in
        -y|--yes)   YES=1 ;;
        -q|--quiet) NEMR_CAT=0 ;;
        -h|--help)  usage; exit 0 ;;
        *) printf 'install_server.sh: unknown option %s\n\n' "$1" >&2; usage >&2; exit 2 ;;
    esac
    shift
done
export NEMR_CAT="${NEMR_CAT:-}"

have() { command -v "$1" >/dev/null 2>&1; }

SYNC_ENV="${NEMR_SYNC_ENV_FILE:-$HOME/.config/nemr/sync.env}"
BUNDLE_DIR="${NEMR_BUNDLE_DIR:-$HOME/.local/share/nemr/bundles}"
LISTEN="${NEMR_SERVER_ADDR:-127.0.0.1:8080}"
DB_URL="${DATABASE_URL:-postgres://nemr:nemr@127.0.0.1:5433/nemr}"

# ---------------------------------------------------------------------------
# Preflight
# ---------------------------------------------------------------------------
MISSING=()
refuse() { MISSING+=("$1"$'\n'"      found: $2"$'\n'"      fix:   $3"); }

preflight() {
    [[ "$(id -u)" -ne 0 ]] || { printf 'Run this as your normal user, not root.\n' >&2; exit 2; }
    have cargo || refuse "the Rust toolchain (the server is built from source here)" \
        "no cargo on PATH" \
        "install rustup: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
    have curl || refuse "curl (used to check the server answers before saying it works)" \
        "no curl on PATH" "sudo apt-get install -y curl"
    if ! have podman && [[ -z "${DATABASE_URL:-}" ]]; then
        refuse "somewhere to put Postgres" \
            "no podman on PATH and no DATABASE_URL in the environment" \
            "either install podman (sudo apt-get install -y podman) and let this start
             postgres:16 for you, or point DATABASE_URL at a Postgres you already run"
    fi
    if (( ${#MISSING[@]} > 0 )); then
        printf '\n%sThe server cannot be installed here yet — nothing has been changed.%s\n\n' \
            "$_S_RED" "$_S_RESET" >&2
        local m; for m in "${MISSING[@]}"; do printf '  needs: %s\n\n' "$m" >&2; done
        exit 1
    fi
}

# ---------------------------------------------------------------------------
# Steps
# ---------------------------------------------------------------------------
STEP_IDS=(postgres settings bundles server)
declare -A STEP_LABEL=(
    [postgres]="Postgres, answering"
    [settings]="the server's settings file, with a pepper"
    [bundles]="the bundle directory"
    [server]="nemr-sync, built and installed"
)
declare -A STEP_STATE=() STEP_DETAIL=()

db_answers() {
    [[ -n "${DATABASE_URL:-}" ]] && return 0
    podman exec nemr-sync-pg pg_isready -U nemr -d nemr >/dev/null 2>&1
}

probe() {
    local id="$1" state=todo detail=""
    case "$id" in
    postgres)
        if [[ -n "${DATABASE_URL:-}" ]]; then
            state=done; detail="DATABASE_URL is set in your environment; this will not start one"
        elif db_answers; then state=done; detail="the nemr-sync-pg container is accepting connections on 127.0.0.1:5433"
        else detail="will start postgres:16 in a rootless podman container on 127.0.0.1:5433"; fi ;;
    settings)
        if [[ -f "$SYNC_ENV" ]] && grep -q '^NEMR_AUTH_PEPPER=' "$SYNC_ENV"; then
            state=done; detail="$SYNC_ENV already has a pepper — it is never regenerated"
        else detail="will write: $SYNC_ENV (mode 0600) with DATABASE_URL, NEMR_SERVER_ADDR, NEMR_BUNDLE_DIR and a NEW random pepper"; fi ;;
    bundles)
        if [[ -d "$BUNDLE_DIR" ]]; then state=done; detail="$BUNDLE_DIR exists"
        else detail="will create: $BUNDLE_DIR (mode 0700)"; fi ;;
    server)
        if [[ -x "$HOME/.local/bin/nemr-sync" ]]; then
            state=rebuild; detail="installed; will rebuild from source and reinstall if it changed"
        else detail="will write: ~/.local/bin/nemr-sync (built here from source)"; fi ;;
    esac
    STEP_STATE["$id"]="$state"; STEP_DETAIL["$id"]="$detail"
}

show_plan() {
    local id n=0
    for id in "${STEP_IDS[@]}"; do probe "$id"; done
    printf '\nnemr install_server — the plan for this machine\n'
    head2 "Steps"
    for id in "${STEP_IDS[@]}"; do
        n=$((n + 1))
        case "${STEP_STATE[$id]}" in
            done)    printf '  %2d. %-44s already done\n' "$n" "${STEP_LABEL[$id]}" ;;
            rebuild) printf '  %2d. %-44s check and update\n' "$n" "${STEP_LABEL[$id]}" ;;
            *)       printf '  %2d. %-44s WILL DO\n' "$n" "${STEP_LABEL[$id]}" ;;
        esac
        note "${STEP_DETAIL[$id]}"
    done

    head2 "Files it writes"
    printf '  %-44s mode 0600 — holds the pepper\n' "$SYNC_ENV"
    printf '  %-44s mode 0700 — the bundles, opaque to this server (E-16)\n' "$BUNDLE_DIR"
    printf '  ~/.local/bin/nemr-sync\n'
    printf '  %s\n' "$LOG"

    head2 "Privileged actions"
    if have podman || [[ -n "${DATABASE_URL:-}" ]]; then
        printf '  none — podman is already here, and it runs rootless\n'
    else
        printf '  apt-get install -y podman   (once; it then runs rootless, as your user)\n'
    fi

    head2 "What it downloads"
    printf '  docker.io   postgres:16, into a rootless podman container\n'
    printf '  crates.io   the Rust dependencies, to build nemr-sync here from this source\n'

    head2 "What it will not do"
    printf '  copy any credential out of your environment into a file — this writes\n'
    printf '  NEMR_BUNDLE_DIR and nothing else about storage (E-20). For an object store,\n'
    printf '  run `nemr server configure`, which asks and writes all of it into sync.env\n'
    printf '  give you TLS, a reverse proxy, backups or a unit: one host, foreground process\n'
    printf '  regenerate a pepper that already exists — changing it locks out every account\n'
}

do_step() {
    local id="$1" label="${STEP_LABEL[$1]}" rc=0
    if [[ "${STEP_STATE[$id]}" == done ]]; then tick "$label — already done"; return 0; fi
    FAILED_STEP="$label"
    case "$id" in
    postgres)
        logged_long ./scripts/setup_sync_test_db.sh || rc=$?
        (( rc == 0 )) && tick "$label — postgres:16 on 127.0.0.1:5433" ;;
    settings)
        # umask first: the file must never exist, even for an instant, in a mode
        # the server would refuse (settings::check_mode) or a reader could use.
        ( umask 077
          mkdir -p "$(dirname "$SYNC_ENV")"
          {
            printf '# nemr sync server settings — written by scripts/install_server.sh\n'
            printf '# The pepper below is a secret: back this file up, and never change it.\n'
            printf '# Changing it invalidates every account on this server (E-19, F-89).\n'
            printf 'DATABASE_URL=%s\n' "$DB_URL"
            printf 'NEMR_SERVER_ADDR=%s\n' "$LISTEN"
            printf 'NEMR_BUNDLE_DIR=%s\n' "$BUNDLE_DIR"
            printf 'NEMR_AUTH_PEPPER=%s\n' "$(head -c 32 /dev/urandom | base64 -w0)"
          } >"$SYNC_ENV"
        ) || rc=$?
        (( rc == 0 )) && tick "$label — $SYNC_ENV, mode $(stat -c %a "$SYNC_ENV")" ;;
    bundles)
        mkdir -p "$BUNDLE_DIR" && chmod 0700 "$BUNDLE_DIR" || rc=$?
        (( rc == 0 )) && tick "$label — $BUNDLE_DIR" ;;
    server)
        local before="" after=""
        [[ -x "$HOME/.local/bin/nemr-sync" ]] && before="$(sha256sum "$HOME/.local/bin/nemr-sync" | cut -d' ' -f1)"
        logged_long cargo build --release -p nemr-sync -p nemr-cloud || rc=$?
        if (( rc == 0 )); then
            mkdir -p "$HOME/.local/bin"
            logged install -m 0755 target/release/nemr-sync "$HOME/.local/bin/nemr-sync" || rc=$?
            after="$(sha256sum "$HOME/.local/bin/nemr-sync" | cut -d' ' -f1)"
            if (( rc == 0 )); then
                if [[ "$before" == "$after" ]]; then tick "$label — already current"
                else tick "$label — installed (${after:0:12})"; fi
            fi
        fi ;;
    esac
    if (( rc != 0 )); then cross "$label"; exit "$rc"; fi
    FAILED_STEP=""
}

steps_trap
preflight
show_plan
nemr_consent "$YES" "./scripts/install_server.sh"

open_log
head2 "Installing"
for id in "${STEP_IDS[@]}"; do do_step "$id"; done

head2 "Verifying"
note "a server is installed when it answers, not when the build exits zero"
FAILED_STEP="the server did not answer"
"$HOME/.local/bin/nemr-sync" >>"$LOG" 2>&1 &
SERVER_PID=$!
# The shared helper, not a hand-rolled poll: it watches the process as well as
# the port, so a server that dies at startup is reported as dead rather than as
# a timeout, and it prints the log when it gives up (F-95). This was a
# hand-rolled loop until the wait-discipline gate — unrun since CI stopped —
# caught it on 2026-09-10.
health=""
if wait_for_service "nemr-sync" "$SERVER_PID" "$LOG" 30 \
        curl -fsS "http://${LISTEN}/health"; then
    health=ok
fi
kill "$SERVER_PID" 2>/dev/null || true
wait "$SERVER_PID" 2>/dev/null || true
if [[ "$health" == "ok" ]]; then
    tick "the server bound ${LISTEN}, opened its store and answered /health"
else
    cross "the server did not answer on ${LISTEN}"
    exit 1
fi
FAILED_STEP=""

cat <<EOF

The sync server is installed.

  nemr-sync                              run it (settings come from $SYNC_ENV)
  NEMR_SERVER_URL=http://${LISTEN} nemr register    point a client at it

Back up $SYNC_ENV. The pepper in it is not recoverable, and
changing it locks every account out of this server.
This is one host with no TLS: put it behind something that terminates TLS
before anyone but you uses it.
EOF
