#!/usr/bin/env bash
#
# Build and import the base image with BuildKit (daemonless, rootless).
#
# Scripted from the README's Milestone 2 procedure so CI and a developer run the
# same path — and so the "no Docker daemon is involved" claim (AC-2.1, NFR-01)
# is exercised rather than asserted. buildkitd runs under rootlesskit as the
# invoking user; `docker`/`dockerd` are not installed and are not required.
#
#   ./scripts/build_base_image.sh
#
# Idempotent: re-running rebuilds and re-imports, overwriting the same tag.

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

# The version lives in src/config.rs; this reads it (F-124). NEMR_BASE_IMAGE
# still overrides, for building a NEW version before the constant moves.
. "$(dirname "${BASH_SOURCE[0]}")/lib/base_image.sh"
IMAGE="${NEMR_BASE_IMAGE:-$(nemr_base_image)}"

# Reproducibility (F-74). Both halves are load-bearing, measured rather than
# assumed: three cold builds with these flags produced one digest, and two cold
# builds without SOURCE_DATE_EPOCH produced two different ones.
#
#   SOURCE_DATE_EPOCH + rewrite-timestamp  normalises file mtimes.
#   the Dockerfile's residue cleanup       removes files that embed the build
#                                          time in their CONTENTS (apt and npm
#                                          logs, ldconfig and V8 caches), which
#                                          no timestamp rewriting can fix.
#
# A fixed epoch rather than the commit date: the image's content does not depend
# on when this repository was last touched, and using a moving value would
# reintroduce exactly the variance this removes.
SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-1700000000}"
OUT="${TMPDIR:-/tmp}/nemr-base.tar"
export PATH="$HOME/.local/bin:$PATH"
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
export BUILDKIT_HOST="${BUILDKIT_HOST:-unix://${XDG_RUNTIME_DIR}/buildkit/buildkitd.sock}"
export CONTAINERD_ADDRESS="${CONTAINERD_ADDRESS:-${XDG_RUNTIME_DIR}/containerd/containerd.sock}"

command -v buildctl >/dev/null || {
    echo "buildctl not found on PATH. Install BuildKit (see README, 'Build tool: BuildKit via buildctl')." >&2
    exit 2
}

# Start a rootless buildkitd if one is not already answering. On a developer host
# it is usually already running; on a fresh runner it is not.
if [[ ! -S "${BUILDKIT_HOST#unix://}" ]]; then
    echo "==> Starting rootless buildkitd"
    command -v buildkitd >/dev/null || {
        echo "buildkitd not found on PATH." >&2
        exit 2
    }
    mkdir -p "${XDG_RUNTIME_DIR}/buildkit"
    rootlesskit \
        --state-dir="${XDG_RUNTIME_DIR}/buildkitd-rootless" \
        --net=slirp4netns --mtu=65520 --disable-host-loopback \
        --copy-up=/etc --copy-up=/run --propagation=rslave \
        buildkitd --oci-worker-no-process-sandbox \
        --root "$HOME/.local/share/buildkit" \
        --addr "$BUILDKIT_HOST" \
        >"${XDG_RUNTIME_DIR}/buildkitd.log" 2>&1 &
    for _ in $(seq 1 30); do
        [[ -S "${BUILDKIT_HOST#unix://}" ]] && break
        sleep 1
    done
    [[ -S "${BUILDKIT_HOST#unix://}" ]] || {
        echo "buildkitd did not come up; log:" >&2
        tail -30 "${XDG_RUNTIME_DIR}/buildkitd.log" >&2 || true
        exit 1
    }
fi

# --no-cache by DEFAULT, because this script's output is a digest that gets
# recorded in image/digests/ and trusted for ever afterwards (F-85).
#
# The Dockerfile's apt line is unpinned by design (see its own comment on the
# snapshot trade-off), so a cached layer holds whatever the archive served on the
# day it was built. Reusing it produces a digest that a fresh builder — CI, or
# anyone else — cannot reproduce, and the failure surfaces later as
# check_base_image_reproducible.sh reporting a mismatch on an image that is not
# actually irreproducible. A confusing failure a long way from its cause.
#
# This costs nothing in CI, which has no cache on a fresh runner. It costs a full
# rebuild locally, so NEMR_BUILD_CACHE=1 opts out for iteration — and says so, so
# a surprising digest is attributable rather than mysterious.
CACHE_FLAG="--no-cache"
if [[ -n "${NEMR_BUILD_CACHE:-}" ]]; then
    CACHE_FLAG=""
    echo "==> NEMR_BUILD_CACHE=1: reusing cached layers."
    echo "    The resulting digest may not be reproducible on a clean builder."
    echo "    Do NOT record it in image/digests/ — rebuild without this first."
fi

echo "==> Building $IMAGE with BuildKit (no Docker daemon)"
BUILD_LOG="$(mktemp)"
trap 'rm -f "$BUILD_LOG"' EXIT
set -o pipefail
buildctl build \
    --frontend dockerfile.v0 \
    --local context=image \
    --local dockerfile=image \
    ${CACHE_FLAG} \
    --opt "build-arg:SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH}" \
    --output "type=oci,dest=${OUT},name=${IMAGE},rewrite-timestamp=true" 2>&1 | tee "$BUILD_LOG"

# CONTROL: prove --no-cache actually did something.
#
# Asking for it and getting cached layers anyway is silent, and it makes every
# downstream claim worthless: two "cold" builds agree trivially, the digest that
# goes into image/digests/ was produced by whatever the archive served on some
# earlier day, and nothing says so. That happened — caught only by noticing one
# build took half as long as another, which is not a mechanism.
#
# The pinned FROM blob is legitimately reused (--no-cache does not re-download a
# digest-pinned base), so only RUN steps are checked.
if [[ -z "${NEMR_BUILD_CACHE:-}" ]]; then
    cached_runs="$(python3 - "$BUILD_LOG" <<'EOF'
import re, sys
steps, cached = {}, set()
for line in open(sys.argv[1], errors="replace"):
    m = re.match(r"#(\d+)\s+\[[\d/]+\]\s+(.*)", line)
    if m:
        steps[m.group(1)] = m.group(2).strip()
    m = re.match(r"#(\d+)\s+CACHED", line)
    if m:
        cached.add(m.group(1))
print("\n".join(steps[n] for n in sorted(cached) if steps.get(n, "").startswith("RUN")))
EOF
)"
    if [[ -n "$cached_runs" ]]; then
        echo "--no-cache was requested and BuildKit reused cached RUN steps anyway:" >&2
        sed 's/^/    /' <<<"$cached_runs" >&2
        echo "The resulting digest reflects an earlier build's inputs. Refusing to" >&2
        echo "report it as though it were freshly built." >&2
        exit 1
    fi
    echo "    control: no RUN step was cached — this build really ran"
fi

echo "==> Importing into containerd"
ctr images import "$OUT"
ctr images list | grep -F "$IMAGE" || {
    echo "import reported success but the image is not listed" >&2
    exit 1
}

echo "==> Built digest (F-74: reproducible given the same inputs)"
built_digest=$(tar -xOf "$OUT" index.json 2>/dev/null \
    | python3 -c 'import sys,json;print(json.load(sys.stdin)["manifests"][0]["digest"])' 2>/dev/null || echo "<unavailable>")
echo "    $built_digest"
if [[ -n "${NEMR_EXPECT_BASE_DIGEST:-}" ]]; then
    if [[ "$built_digest" == "$NEMR_EXPECT_BASE_DIGEST" ]]; then
        echo "    matches NEMR_EXPECT_BASE_DIGEST"
    else
        echo "    MISMATCH: expected $NEMR_EXPECT_BASE_DIGEST" >&2
        exit 1
    fi
fi

# F-85: the built digest must match the digest recorded for THIS version. A
# mismatch means the image contents changed without a version bump — the exact
# failure that let the two-agent image reuse the 0.1.0 tag. Refuse it here, at
# build time, so the tag can never name two different sets of bytes.
version="${IMAGE##*:}"
ledger="image/digests/${version}"
if [[ -f "$ledger" ]]; then
    recorded="$(tr -d "[:space:]" < "$ledger")"
    if [[ "$built_digest" == "$recorded" ]]; then
        echo "    matches image/digests/${version} (F-85)"
    else
        cat >&2 <<EOF

    F-85 VERSION MISMATCH — the image built for version ${version} is not the
    one recorded for that version.

        recorded (image/digests/${version}): ${recorded}
        just built:                          ${built_digest}

    A version tag must never name two different images. If the image changed on
    purpose, this is a NEW version: bump the tag in src/config.rs (BASE_IMAGE)
    and add image/digests/<new-version> with the built digest. Do NOT overwrite
    ${ledger}.
EOF
        exit 1
    fi
else
    echo "    NOTE: no digest recorded for version ${version} yet." >&2
    echo "          If this is a new version, record it: echo ${built_digest} > ${ledger}" >&2
fi

echo "==> Measured size (AC-2.2)"
ctr images list | awk -v img="$IMAGE" '$1 == img { print "   ", $1, $4 }'
rm -f "$OUT"
