#!/usr/bin/env bash
#
# Install the sync client (WP-K): one binary, many names.
#
# Builds `nemr-cloud` in release mode, copies the artifact (the same
# build-once-copy discipline as install_engine.sh — F-62: `cargo install`
# rebuilds and defeats hash gates), and lays the symlinks the open CLI's
# external-subcommand fallback resolves: `nemr login` execs `nemr-login`, which
# is this binary wearing that name.
#
#   ./scripts/install_sync_client.sh
#
# The open engine works fully without any of this installed — E-11's test.

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

DEST="${NEMR_INSTALL_DIR:-$HOME/.local/bin}"
NAMES=(login logout register sessions push pull release ui)

cargo build --release -p nemr-cloud

mkdir -p "$DEST"
install -m 0755 target/release/nemr-cloud "$DEST/nemr-cloud"
for name in "${NAMES[@]}"; do
    ln -sf "$DEST/nemr-cloud" "$DEST/nemr-$name"
done

echo "installed $DEST/nemr-cloud and symlinks: ${NAMES[*]/#/nemr-}"
echo "the open CLI resolves them: try  nemr login"
