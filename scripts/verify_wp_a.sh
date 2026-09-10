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
# Count assertion, not just an exit status.
#
# `cargo test` exits 0 when it runs ZERO tests — a mistyped name filter prints
# "running 0 tests / test result: ok" and passes. And a host-backed test that
# returns early under NEMR_TEST_UNIT_ONLY is counted as *passed*, so a suite can
# report "16 passed" while 14 of them did nothing. Both are green-over-nothing in
# the harness rather than the code, so this gate asserts on counts: every test
# that exists must run, and none may skip.
regression_total=$(cargo test --test regression -- --list 2>/dev/null | grep -c ': test$')
# F-65: under `set -euo pipefail`, `x=$(failing-cmd)` aborts the script at the
# assignment — so `regression_status=$?` never ran and the tail -40 below it was
# unreachable. This verification script would die printing nothing on exactly
# the failure it exists to report. An assignment in an `if` condition is exempt
# from -e, so the output survives.
if ! regression_output=$(cargo test --test regression -- --test-threads=1 --nocapture 2>&1); then
    printf '%s\n' "$regression_output" | grep -E '^test result:' || true
    printf '%s\n' "$regression_output" | tail -40
    echo "    regression suite FAILED" >&2
    exit 1
fi
printf '%s\n' "$regression_output" | grep -E '^test result:' || true

regression_passed=$(printf '%s\n' "$regression_output" \
    | sed -n 's/.*test result: ok\. \([0-9]*\) passed.*/\1/p' | head -1)
# The LEDGER, not the output. The old grep looked for a message libtest
# captures for a passing test — and every skipped test passes — so it read 0
# unless the run happened to pass --nocapture. The suite now writes one line per
# skip to a file, and reads back cleanly whatever the runner asked for
# (tests/common/mod.rs: unit_only_ledger).
skip_ledger="$(find target -name unit-only-skips.txt -newer Cargo.toml 2>/dev/null | head -1)"
regression_skipped=$(printf '%s\n' "$regression_output" \
    | grep -c 'NEMR-SKIP' || true)
if [[ -n "$skip_ledger" && -s "$skip_ledger" ]]; then
    regression_skipped=$(sort -u "$skip_ledger" | wc -l)
fi

if [[ "${regression_passed:-0}" -ne "${regression_total:-0}" ]]; then
    echo "    only ${regression_passed:-0} of ${regression_total:-0} regression tests ran." >&2
    echo "    A partial run is not a pass — check for a name filter or a build error." >&2
    exit 1
fi
if [[ "${regression_skipped:-0}" -ne 0 ]]; then
    echo "    ${regression_skipped} regression test(s) did NOT RUN (NEMR_TEST_UNIT_ONLY)." >&2
    echo "    cargo reports them as passed; they checked nothing. This gate verifies the" >&2
    echo "    HOST-backed guarantees, so a skipped run is not a pass. They were:" >&2
    [[ -n "$skip_ledger" ]] && sed 's/^/      /' "$skip_ledger" >&2
    exit 1
fi
echo "    ok — all ${regression_total} regression tests ran, none skipped"

echo "==> E2E smoke test (unprivileged)"
./scripts/e2e_smoke_test.sh

echo
echo "WP A verification complete."
