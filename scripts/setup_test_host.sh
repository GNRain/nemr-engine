#!/usr/bin/env bash
#
# Install (or update) the privileged volume helper and its sudoers grant.
#
# This is the one privileged step in setting up a Nemr host: it copies a
# root-owned binary to /usr/local/libexec and installs a NOPASSWD sudoers rule.
# It is deliberately a separate, auditable script rather than something the
# engine does for itself (NFR-05: the engine never escalates its own privilege).
#
# Run it after any change to deploy/nemr-volume/ — the engine checks the
# installed helper's protocol version and refuses to run against a stale one, so
# a source change that is not installed here is caught rather than silently
# ignored.
#
#   sudo ./scripts/setup_test_host.sh
#
# It is idempotent: re-running it rebuilds and reinstalls the current helper.

set -euo pipefail

HELPER_SRC_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../deploy/nemr-volume" && pwd)"
SUDOERS_SRC="$(cd "$(dirname "${BASH_SOURCE[0]}")/../deploy/sudoers.d" && pwd)/nemr-volume"
HELPER_DEST="/usr/local/libexec/nemr-volume"
SUDOERS_DEST="/etc/sudoers.d/nemr-volume"

# The account that runs the engine. Defaults to the user who invoked sudo, which
# is almost always right; override with NEMR_ACCOUNT=<user> for an unusual setup.
ACCOUNT="${NEMR_ACCOUNT:-${SUDO_USER:-}}"

if [[ "$(id -u)" -ne 0 ]]; then
    echo "This script installs a root-owned binary and a sudoers rule; run it with sudo." >&2
    exit 1
fi
if [[ -z "$ACCOUNT" ]]; then
    echo "Cannot determine the engine account. Set NEMR_ACCOUNT=<user> and retry." >&2
    exit 1
fi

echo "==> Building the helper (release)"
# Build as the invoking user so the target/ tree stays user-owned.
sudo -u "$ACCOUNT" bash -lc "cd '$HELPER_SRC_DIR' && cargo build --release"

echo "==> Installing $HELPER_DEST (root:root, 0755)"
install -o root -g root -m 0755 "$HELPER_SRC_DIR/target/release/nemr-volume" "$HELPER_DEST"

echo "==> Installing $SUDOERS_DEST (root:root, 0440)"
# Substitute the account into the grant's first field, then validate BEFORE
# installing — a malformed sudoers file can lock everyone out of sudo.
tmp="$(mktemp)"
sed "s/^nemr ALL=/${ACCOUNT} ALL=/" "$SUDOERS_SRC" > "$tmp"
if ! visudo -c -f "$tmp" >/dev/null; then
    echo "Refusing to install: the generated sudoers file did not pass visudo -c." >&2
    rm -f "$tmp"
    exit 1
fi
install -o root -g root -m 0440 "$tmp" "$SUDOERS_DEST"
rm -f "$tmp"

echo "==> Verifying"
installed_version="$("$HELPER_DEST" version 2>/dev/null || true)"
echo "    helper protocol: ${installed_version:-<none>}"
echo "    grant: $(grep -E "ALL=\(root\) NOPASSWD" "$SUDOERS_DEST")"
echo
echo "Done. The hardened helper is installed. Re-run the regression suite:"
echo "    cargo test --test regression -- --test-threads=1"
echo "    ./scripts/e2e_smoke_test.sh"
