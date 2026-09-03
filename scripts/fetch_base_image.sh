#!/usr/bin/env bash
#
# Obtain the base image for provisioning: PULL the published version first,
# build only as a fallback (F-126 — the WSL2 spike's divergence 1).
#
# D-08 part 1 published the image precisely so a fresh host does not need
# BuildKit and a long build — the cross-VM run measured that cost at ~2 hours —
# yet until this script, provisioning built anyway. `build_base_image.sh`
# remains the developer/publish tool; this is what setup uses.
#
#   ./scripts/fetch_base_image.sh
#
# Every path ends in exactly one of two states: the image present in containerd
# at the digest recorded in image/digests/<version>, or a refusal naming both
# digests and the remedy. The failure modes, decided rather than discovered:
#
#   present at a different digest    REFUSE. Something replaced the tag locally;
#                                    re-pulling over it would be repairing what
#                                    should be investigated.
#   pull ok, digest != recorded      REFUSE, and do NOT fall back to the build:
#                                    the registry drifted (the F-85 class
#                                    check_base_image_published.sh exists for),
#                                    and a local build would mask exactly that.
#   pull fails (offline, 403, 404)   Fall back to build_base_image.sh — whose
#                                    F-85 gate, plus the recheck here, refuses
#                                    any digest that is not the recorded one.
#                                    Fail closed: bundles pin the base digest,
#                                    so a host running a divergent image exports
#                                    bundles no other machine can restore. The
#                                    remedy is network to ghcr.io, never "use
#                                    whatever built". Honest limit: the apt
#                                    layer is unpinned by design (see the
#                                    Dockerfile), so once the Debian archive
#                                    moves past the recorded build's inputs the
#                                    fallback CANNOT reproduce the digest — it
#                                    works only in that window, and refuses
#                                    plainly outside it.

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

. scripts/lib/base_image.sh
IMAGE="${NEMR_BASE_IMAGE:-$(nemr_base_image)}"
VERSION="${IMAGE##*:}"
LEDGER="image/digests/${VERSION}"
# Test seam (same pattern as check_base_image_published.sh's F-86/F-87 seams):
# the suite substitutes a shim builder so every branch is provable offline.
BUILDER="${NEMR_TEST_BUILDER:-./scripts/build_base_image.sh}"

export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
export CONTAINERD_ADDRESS="${CONTAINERD_ADDRESS:-${XDG_RUNTIME_DIR}/containerd/containerd.sock}"

GREEN=$'\033[32m'; RED=$'\033[31m'; RESET=$'\033[0m'
ok() { printf '    %sok%s   %s\n' "$GREEN" "$RESET" "$1"; }

# The SOLE refusal site. The regression suite's neutered-copy control depends
# on every refusal flowing through this exit — do not add another.
refuse() {
    printf '%sREFUSED%s — %s\n' "$RED" "$RESET" "$1" >&2
    exit 1
}

[[ -f "$LEDGER" ]] || refuse "no recorded digest for version ${VERSION} (${LEDGER} is missing).
    Provisioning only ever installs published, recorded bytes. If you are
    developing a NEW version, use ./scripts/build_base_image.sh directly and
    record its digest per image/digests/README.md."
RECORDED="$(tr -d '[:space:]' < "$LEDGER")"

present() {
    ctr -n default images ls 2>/dev/null \
        | awk -v img="$IMAGE" '$1 == img { print $3 }' | head -1
}

# containerd must be answering, or "absent" below would mean "unreachable".
ctr -n default images ls > /dev/null 2>&1 \
    || refuse "containerd is not answering at ${CONTAINERD_ADDRESS}.
    Start it first (setup_host.sh does) — this script cannot tell a missing
    image from a missing daemon, so it refuses rather than guessing."

have="$(present)"
if [[ "$have" == "$RECORDED" ]]; then
    ok "${IMAGE} already present at the recorded digest"
    exit 0
elif [[ -n "$have" ]]; then
    refuse "${IMAGE} is already present at a DIFFERENT digest.
        recorded (${LEDGER}): ${RECORDED}
        present locally:      ${have}
    A version tag must name one set of bytes (F-85). Something put different
    bytes under this tag — find out what before removing it:
        ctr -n default images rm ${IMAGE}
    then re-run this script."
fi

echo "==> Pulling ${IMAGE} (the published bytes — D-08; no build needed)"
if pull_out="$(ctr -n default images pull --platform linux/amd64 "$IMAGE" 2>&1)"; then
    have="$(present)"
    [[ "$have" == "$RECORDED" ]] || refuse "the registry served ${VERSION} at a digest that is NOT the recorded one.
        recorded (${LEDGER}): ${RECORDED}
        served by registry:   ${have:-<none>}
    The published tag drifted (F-85) — every bundle built against ${VERSION}
    depends on the recorded bytes. NOT falling back to a local build, which
    would mask the drift. Run ./scripts/check_base_image_published.sh and
    restore the original bytes at the tag."
    ok "pulled at the recorded digest ${RECORDED}"
    exit 0
fi

echo "    pull failed:" >&2
printf '%s\n' "$pull_out" | tail -5 | sed 's/^/        /' >&2
echo "==> Falling back to a local build — it must REPRODUCE the recorded digest"
"$BUILDER" || refuse "the fallback build failed, or refused its own digest check.
        recorded (${LEDGER}): ${RECORDED}
    This host cannot obtain the base image: the registry is unreachable and a
    local build did not reproduce the published bytes (once the Debian archive
    moves, it cannot — the apt layer is unpinned by design). A divergent image
    would export bundles no other machine can restore, so this fails closed.
    Remedy: restore network access to ghcr.io and re-run."

# Self-contained recheck, independent of the builder's internals: whatever the
# builder said, what is now present must be the recorded bytes.
have="$(present)"
[[ "$have" == "$RECORDED" ]] || refuse "the fallback build completed but the image present is NOT the recorded one.
        recorded (${LEDGER}): ${RECORDED}
        present after build:  ${have:-<none>}
    Failing closed for the same reason as above: a divergent base image breaks
    bundle portability. Remedy: restore network access to ghcr.io and re-run."
ok "fallback build reproduced the recorded digest ${RECORDED}"
