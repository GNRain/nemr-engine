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

IMAGE="${NEMR_BASE_IMAGE:-ghcr.io/gnrain/nemr-base:0.1.0}"

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

echo "==> Building $IMAGE with BuildKit (no Docker daemon)"
buildctl build \
    --frontend dockerfile.v0 \
    --local context=image \
    --local dockerfile=image \
    --opt "build-arg:SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH}" \
    --output "type=oci,dest=${OUT},name=${IMAGE},rewrite-timestamp=true"

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

echo "==> Measured size (AC-2.2)"
ctr images list | awk -v img="$IMAGE" '$1 == img { print "   ", $1, $4 }'
rm -f "$OUT"
