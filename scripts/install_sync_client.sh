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
NAMES=(login logout register sessions push pull release ui server)

# Built TOGETHER with the server, deliberately, and every script that hashes
# this binary builds it the same way (F-7). Measured: `-p nemr-cloud` alone
# and `-p nemr-sync -p nemr-cloud` together produce different nemr-cloud
# binaries — cargo unifies features across the packages in one invocation —
# and cargo swaps between the two cached artifacts without recompiling. A
# hash gate is only as good as the build command being the same on both
# sides, so this is the one command: here, in docs/ui-acceptance.sh and in
# scripts/sync_acceptance.sh.
cargo build --release -p nemr-sync -p nemr-cloud

mkdir -p "$DEST"
install -m 0755 target/release/nemr-cloud "$DEST/nemr-cloud"
for name in "${NAMES[@]}"; do
    ln -sf "$DEST/nemr-cloud" "$DEST/nemr-$name"
done

# The hash gate (F-7, the same discipline as install_engine.sh): the installed
# binary is provably the one this working tree produced, and every name on
# PATH resolves to it. A `nemr-ui` from an older install, or one shadowed by
# another directory on PATH, is named here rather than discovered by an
# acceptance that passed from target/release while the install was stale.
installed=$(sha256sum "$DEST/nemr-cloud" | cut -d' ' -f1)
built=$(sha256sum target/release/nemr-cloud | cut -d' ' -f1)
echo "installed $DEST/nemr-cloud"
echo "  sha256 $installed"
echo "  built  $built"
[[ "$installed" == "$built" ]] || { echo "the installed binary does not match the build — the copy failed" >&2; exit 1; }
for name in "${NAMES[@]}"; do
    on_path=$(command -v "nemr-$name" || true)
    if [[ -z "$on_path" ]]; then
        echo "note: $DEST is not on PATH, so 'nemr $name' will not resolve nemr-$name" >&2
        break
    fi
    if [[ "$(readlink -f "$on_path")" != "$(readlink -f "$DEST/nemr-cloud")" ]]; then
        echo "STALE: 'nemr $name' resolves to $on_path, which is not this install ($DEST/nemr-cloud)" >&2
        exit 1
    fi
done
echo "symlinks: ${NAMES[*]/#/nemr-} — every one on PATH resolves to this build"
echo "the open CLI resolves them: try  nemr login"
