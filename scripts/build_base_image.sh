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

IMAGE="${NEMR_BASE_IMAGE:-docker.io/nemr/base:0.1.0}"
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
    --output "type=oci,dest=${OUT},name=${IMAGE}"

echo "==> Importing into containerd"
ctr images import "$OUT"
ctr images list | grep -F "$IMAGE" || {
    echo "import reported success but the image is not listed" >&2
    exit 1
}

echo "==> Measured size (AC-2.2)"
ctr images list | awk -v img="$IMAGE" '$1 == img { print "   ", $1, $4 }'
rm -f "$OUT"
