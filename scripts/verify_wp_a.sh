#!/usr/bin/env bash
#
# Unprivileged verification for WP A: the full regression suite plus the E2E
# smoke test, against the *installed* helper.
#
# Run it AFTER installing the hardened helper:
#
#   sudo ./scripts/setup_test_host.sh   # privileged: installs the helper
#   ./scripts/verify_wp_a.sh            # unprivileged: this script
#
# It does not use sudo itself. The regression suite refuses to run unless the
# installed helper is byte-identical to the built source (TEST-01), so a stale
# install fails loudly here rather than passing against a binary nobody runs.

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

export CONTAINERD_ADDRESS="${CONTAINERD_ADDRESS:-$XDG_RUNTIME_DIR/containerd/containerd.sock}"

echo "==> Helper freshness (installed must match built source)"
built="deploy/nemr-volume/target/release/nemr-volume"
if [[ ! -x "$built" ]]; then
    (cd deploy/nemr-volume && cargo build --release)
fi
installed_hash="$(sha256sum /usr/local/libexec/nemr-volume | cut -d' ' -f1)"
built_hash="$(sha256sum "$built" | cut -d' ' -f1)"
if [[ "$installed_hash" != "$built_hash" ]]; then
    echo "    MISMATCH — installed helper is not the built source." >&2
    echo "    installed $installed_hash" >&2
    echo "    built     $built_hash" >&2
    echo "    fix: sudo ./scripts/setup_test_host.sh" >&2
    exit 1
fi
echo "    ok ($installed_hash)"

echo "==> Unit tests (engine + helper)"
cargo test --lib --quiet
(cd deploy/nemr-volume && cargo test --quiet)

echo "==> Regression suite (host-backed, serial)"
cargo test --test regression --quiet -- --test-threads=1

echo "==> E2E smoke test (unprivileged)"
./scripts/e2e_smoke_test.sh

echo
echo "WP A verification complete."
