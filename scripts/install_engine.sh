#!/usr/bin/env bash
# Install the nemr engine binary so the hash gate can verify it (F-62).
#
# Deliberately NOT `cargo install --path .`: cargo install rebuilds in its own
# target directory and emits a binary that differs byte-for-byte from
# `target/release/nemr` built from identical source (an embedded path, ~40 bytes
# here). That difference is indistinguishable from "you installed a stale
# build", so a hash gate over a cargo-installed binary can only ever be red.
#
# Building once and copying the artifact makes the installed binary provably the
# one this working tree produced — the same property scripts/setup_test_host.sh
# gives the privileged helper.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
DEST="${NEMR_INSTALLED_BIN:-$HOME/.local/bin/nemr}"
DAEMON_DEST="$(dirname "$DEST")/nemrd"

# Both binaries, from one build, installed side by side. The CLI autostarts the
# daemon from beside itself, so they must be the same build (E-09).
cargo build --release --bin nemr --bin nemrd
mkdir -p "$(dirname "$DEST")"
install -m 0755 target/release/nemr "$DEST"
install -m 0755 target/release/nemrd "$DAEMON_DEST"

# Stop any running daemon so the next command autostarts the freshly installed
# one. This is how a stale daemon is prevented structurally rather than
# discovered the hard way — the CLI protocol handshake catches incompatible
# versions, and stopping here catches same-version behavioural drift.
if [[ -n "${XDG_RUNTIME_DIR:-}" && -S "$XDG_RUNTIME_DIR/nemr/nemrd.sock" ]]; then
    # Every nemrd of this user, wherever it was started from (F-9: a daemon
    # autostarted from a build tree by a test kept the socket while this
    # matched only $DAEMON_DEST, and the fresh install never took over).
    for pid in $(pgrep -x nemrd -u "$(id -u)" 2>/dev/null || true); do
        kill "$pid" 2>/dev/null || true
    done
    rm -f "$XDG_RUNTIME_DIR/nemr/nemrd.sock"
    echo "  stopped the running daemon; the next command will start the new one"
fi

echo "installed $DEST"
echo "  sha256 $(sha256sum "$DEST" | cut -d' ' -f1)"
echo "  built  $(sha256sum target/release/nemr | cut -d' ' -f1)"
echo "installed $DAEMON_DEST"
echo "  sha256 $(sha256sum "$DAEMON_DEST" | cut -d' ' -f1)"

command -v nemr >/dev/null && [ "$(command -v nemr)" = "$DEST" ] || {
    echo "note: $DEST is not the 'nemr' on PATH ($(command -v nemr || echo none))." >&2
    echo "      Add $(dirname "$DEST") to PATH, or the smoke test will run a different binary." >&2
}
