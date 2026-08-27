#!/usr/bin/env bash
#
# Bring up the Postgres the nemr-sync integration tests need (WP-J), in a
# rootless podman container matching CI's `postgres:16` service. Idempotent and
# re-runnable.
#
# WHY A CONTAINER, NOT A HOST INSTALL. It pins the exact version CI runs
# (postgres:16), so a local pass and a CI pass are the same claim; it leaves no
# system service behind; and it matches the project's rootless posture. Podman's
# rootless dependencies (slirp4netns, uidmap) are already installed for rootless
# containerd, so the only new package is podman itself.
#
# WHAT YOU DO NOT HAVE TO DO. The role and database are created by the image
# from the env vars below — no `createuser`/`createdb`. The schema is created by
# the test harness itself: `connect_and_migrate` runs the embedded migrations on
# connect, so there is no manual `sqlx migrate` step either.
#
#   ./scripts/setup_sync_test_db.sh          # start (installs podman if missing)
#   ./scripts/setup_sync_test_db.sh --stop   # stop and remove the container
#
# Then run the suite:
#   export DATABASE_URL=postgres://nemr:nemr@127.0.0.1:5433/nemr
#   cargo test -p nemr-sync
#
# Port 5433 (not 5432) so this never collides with a system Postgres.

set -euo pipefail

NAME=nemr-sync-pg
PORT=5433
IMAGE=docker.io/library/postgres:16
DB_URL="postgres://nemr:nemr@127.0.0.1:${PORT}/nemr"

GREEN=$'\033[32m'; YELLOW=$'\033[33m'; RESET=$'\033[0m'
ok()   { printf '    %sok%s   %s\n' "$GREEN" "$RESET" "$1"; }
warn() { printf '    %swarn%s %s\n' "$YELLOW" "$RESET" "$1"; }

if [[ "${1:-}" == "--stop" ]]; then
    podman rm -f "$NAME" >/dev/null 2>&1 && ok "removed $NAME" || ok "$NAME not running"
    exit 0
fi

# Podman, installed once. Needs root; nothing else here does.
if command -v podman >/dev/null 2>&1; then
    ok "podman already present ($(podman --version 2>/dev/null || echo 'version unknown'))"
else
    echo "==> installing podman (one-time; rootless deps already present)"
    sudo apt-get update
    sudo DEBIAN_FRONTEND=noninteractive apt-get install -y podman
fi

if podman ps --format '{{.Names}}' | grep -qx "$NAME"; then
    ok "$NAME already running"
elif podman ps -a --format '{{.Names}}' | grep -qx "$NAME"; then
    podman start "$NAME" >/dev/null
    ok "started existing $NAME"
else
    echo "==> starting $IMAGE as $NAME on 127.0.0.1:${PORT}"
    podman run -d --name "$NAME" \
        -p "127.0.0.1:${PORT}:5432" \
        -e POSTGRES_USER=nemr \
        -e POSTGRES_PASSWORD=nemr \
        -e POSTGRES_DB=nemr \
        "$IMAGE" >/dev/null
    ok "created $NAME"
fi

# Not ready when the process starts — ready when it accepts connections.
echo "==> waiting for Postgres to accept connections"
for _ in $(seq 1 30); do
    if podman exec "$NAME" pg_isready -U nemr -d nemr >/dev/null 2>&1; then
        ok "Postgres is ready"
        printf '\n%sReady.%s Run the sync-server suite with:\n\n' "$GREEN" "$RESET"
        printf '    export DATABASE_URL=%s\n' "$DB_URL"
        printf '    cargo test -p nemr-sync\n\n'
        printf 'Stop it later with:  ./scripts/setup_sync_test_db.sh --stop\n'
        exit 0
    fi
    sleep 1
done

warn "Postgres did not become ready in 30s; check: podman logs $NAME"
exit 1
