#!/usr/bin/env bash
#
# Publish the base image to GHCR (D-08 part 1).
#
# Run from CI so the published artifact and the digest recorded in this
# repository cannot diverge — the same argument as the binary hash gates: a
# reference that names bytes nobody verified is a reference to nothing.
#
#   GHCR_TOKEN=<token> ./scripts/publish_base_image.sh
#
# Registry credentials are read from $DOCKER_CONFIG, which this script points at
# a private temporary directory. That filename is Docker's convention and
# BuildKit's default lookup path; it is NOT a Docker dependency, and pointing it
# away from ~/.docker keeps the tripwire in F-49 meaningful — if ~/.docker
# appears, something really did create it.
#
# What this establishes that building does not: `check_base_image_reproducible`
# shows the build agrees with itself given the same inputs. Publishing means
# every host gets the SAME bytes without building at all — which is what the
# cross-machine test priced when it spent an hour on BuildKit setup to produce
# an image that should have been a download.

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

IMAGE="${NEMR_BASE_IMAGE:-ghcr.io/gnrain/nemr-base:0.2.0}"
# F-85: digests are recorded per version, one file each, never a single
# shared value.
VERSION="${IMAGE##*:}"
DIGEST_FILE="image/digests/${VERSION}"
SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-1700000000}"

export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
export BUILDKIT_HOST="${BUILDKIT_HOST:-unix://${XDG_RUNTIME_DIR}/buildkit/buildkitd.sock}"
export PATH="$HOME/.local/bin:$PATH"

: "${GHCR_TOKEN:?GHCR_TOKEN is required (CI supplies GITHUB_TOKEN)}"
GHCR_USER="${GHCR_USER:-${GITHUB_ACTOR:-x-access-token}}"

command -v buildctl >/dev/null || { echo "buildctl not found on PATH" >&2; exit 2; }
[[ -S "${BUILDKIT_HOST#unix://}" ]] || { echo "buildkitd not answering at $BUILDKIT_HOST" >&2; exit 2; }

# Auth, in a private directory that is removed on exit. Never ~/.docker.
export DOCKER_CONFIG
DOCKER_CONFIG="$(mktemp -d)"
trap 'rm -rf "$DOCKER_CONFIG"' EXIT
chmod 700 "$DOCKER_CONFIG"
python3 - "$GHCR_USER" "$GHCR_TOKEN" > "$DOCKER_CONFIG/config.json" <<'PY'
import base64, json, sys
user, token = sys.argv[1], sys.argv[2]
auth = base64.b64encode(f"{user}:{token}".encode()).decode()
json.dump({"auths": {"ghcr.io": {"auth": auth}}}, sys.stdout)
PY
chmod 600 "$DOCKER_CONFIG/config.json"

echo "==> Building and pushing $IMAGE"
# Same reproducibility flags as the local build, so what is published is the
# image the repository describes rather than a differently-built one.
buildctl build \
    --frontend dockerfile.v0 \
    --local context=image \
    --local dockerfile=image \
    --opt "build-arg:SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH}" \
    --output "type=image,name=${IMAGE},push=true,rewrite-timestamp=true" \
    --metadata-file /tmp/nemr-publish-metadata.json

published=$(python3 -c '
import json
with open("/tmp/nemr-publish-metadata.json") as f:
    print(json.load(f).get("containerimage.digest", ""))
')
rm -f /tmp/nemr-publish-metadata.json

if [[ ! "$published" =~ ^sha256:[0-9a-f]{64}$ ]]; then
    echo "the push reported no usable digest (${published:-empty}); refusing to record it" >&2
    exit 1
fi

echo "==> Published digest: $published"

# The per-version divergence check (F-85). A version tag names a specific set of
# bytes forever: publishing DIFFERENT bytes under an existing version is refused
# outright — that is the "success signal over the wrong artifact" this whole
# change exists to stop. A version with no recorded digest is a genuinely new
# version and prints the value to record.
if [[ -f "$DIGEST_FILE" ]]; then
    recorded=$(tr -d '[:space:]' < "$DIGEST_FILE")
    if [[ "$recorded" == "$published" ]]; then
        echo "    matches $DIGEST_FILE"
    else
        cat >&2 <<EOF

VERSION TAG REUSED FOR DIFFERENT BYTES — refusing to publish.

    version ${VERSION} is recorded as: $recorded
    this build produced:               $published

A version tag must never name two different images (F-85). This is NOT a digest
to re-record — it means the image changed without a version bump. Bump BASE_IMAGE
in src/config.rs to a new version and add image/digests/<new-version>. The
existing ${DIGEST_FILE} is immutable and must not be edited.

EOF
        exit 1
    fi
else
    cat >&2 <<EOF

New version ${VERSION} — no digest recorded yet. Record it and commit:

    echo $published > $DIGEST_FILE

EOF
    exit 1
fi
