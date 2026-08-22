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

cargo build --release --bin nemr
mkdir -p "$(dirname "$DEST")"
install -m 0755 target/release/nemr "$DEST"

echo "installed $DEST"
echo "  sha256 $(sha256sum "$DEST" | cut -d' ' -f1)"
echo "  built  $(sha256sum target/release/nemr | cut -d' ' -f1)"

command -v nemr >/dev/null && [ "$(command -v nemr)" = "$DEST" ] || {
    echo "note: $DEST is not the 'nemr' on PATH ($(command -v nemr || echo none))." >&2
    echo "      Add $(dirname "$DEST") to PATH, or the smoke test will run a different binary." >&2
}
