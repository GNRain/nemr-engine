#!/usr/bin/env bash
#
# The published base image must be pullable WITHOUT an account (D-08, E-11).
#
# Publishing is not the goal; being obtainable is. D-08 chose GHCR on the
# argument that the image is "obtainable without an account", and E-11's test —
# can someone use the open half productively without ever paying? — depends on
# it. A package that exists but answers 403 to an anonymous client satisfies
# neither, and looks identical to success from the publishing side: the push
# succeeds, the digest is recorded, and nothing says the artifact is unreachable.
#
# GHCR packages published with GITHUB_TOKEN default to PRIVATE. Visibility is a
# one-time setting in the repository's package settings, not something the
# workflow can set, so this check exists to make the gap loud rather than
# assumed.
#
#   ./scripts/check_base_image_published.sh
#
# Anonymous by construction: it deliberately sends no credential, because "it
# works for me" is exactly the failure mode here.

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

IMAGE_REPO="${NEMR_BASE_REPO:-gnrain/nemr-base}"
IMAGE_TAG="${NEMR_BASE_TAG:-0.2.0}"
DIGEST_FILE="image/digests/${IMAGE_TAG}"
ACCEPT='application/vnd.oci.image.manifest.v1+json,application/vnd.oci.image.index.v1+json,application/vnd.docker.distribution.manifest.v2+json,application/vnd.docker.distribution.manifest.list.v2+json'

RED=$'\033[31m'; GREEN=$'\033[32m'; RESET=$'\033[0m'

anon_token() {
    curl -fsS --max-time 20 "https://ghcr.io/token?service=ghcr.io&scope=repository:$1:pull" \
        | python3 -c 'import sys,json;print(json.load(sys.stdin).get("token",""))'
}

manifest_status() {
    local repo="$1" ref="$2" token
    token="$(anon_token "$repo" || true)"
    curl -s -o /dev/null -w '%{http_code}' --max-time 20 \
        -H "Authorization: Bearer ${token}" -H "Accept: ${ACCEPT}" \
        "https://ghcr.io/v2/${repo}/manifests/${ref}"
}

# CONTROL first: an unauthenticated client must be able to read a package that
# is genuinely public. Without this, a 403 below could mean "GHCR refuses
# everyone anonymously" rather than "our package is private", and the remedy
# would be wrong.
control_status="$(manifest_status "actions/actions-runner" "latest")"
if [[ "$control_status" != "200" ]]; then
    printf '%sFAIL%s — control: a known-public GHCR package answered HTTP %s anonymously.\n' \
        "$RED" "$RESET" "$control_status"
    printf '       This check cannot distinguish "our package is private" from "anonymous\n'
    printf '       access to GHCR is not working from here", so it is not reporting.\n'
    exit 1
fi
printf '%sok%s        control: a known-public GHCR package is readable anonymously (HTTP 200)\n' \
    "$GREEN" "$RESET"

# Test seam (F-86): force the subject's HTTP status so the message-selection
# branches are provable without a live registry. The control above still runs
# against the real network, so a forced run cannot pass vacuously.
status="${NEMR_TEST_FORCE_STATUS:-$(manifest_status "$IMAGE_REPO" "$IMAGE_TAG")}"
if [[ "$status" != "200" ]]; then
    # 403 and 404 are DIFFERENT failures with different fixes, and conflating
    # them (F-86) sent someone to toggle visibility when the real problem was
    # that the version was never published. Name the actual cause.
    case "$status" in
        403)
            cat >&2 <<EOF

${RED}FAIL${RESET} — ghcr.io/${IMAGE_REPO}:${IMAGE_TAG} exists but is PRIVATE (HTTP 403).

  The package is published but not obtainable without an account, which
  satisfies neither D-08 nor E-11. GHCR packages published with GITHUB_TOKEN
  default to private, and a workflow cannot set its own package visibility.
  Fix once, by hand:

    https://github.com/users/gnrain/packages/container/nemr-base/settings
    -> Danger Zone -> Change visibility -> Public

EOF
            ;;
        404)
            cat >&2 <<EOF

${RED}FAIL${RESET} — ghcr.io/${IMAGE_REPO}:${IMAGE_TAG} is NOT PUBLISHED (HTTP 404).

  The registry has no such version. This is not a visibility problem — the
  image was never pushed, or the push failed. Publish it:

    ./scripts/publish_base_image.sh        (or the publish-base-image workflow)

  If the package's OTHER versions 403 anonymously, its visibility is also
  private — fix that too, at the settings page above.

EOF
            ;;
        *)
            cat >&2 <<EOF

${RED}FAIL${RESET} — ghcr.io/${IMAGE_REPO}:${IMAGE_TAG} is not readable anonymously (HTTP ${status}).

  Unexpected status. The control above confirmed anonymous GHCR access works,
  so this is specific to this package/version. Inspect it by hand before
  assuming a cause.

EOF
            ;;
    esac
    exit 1
fi
printf '%sok%s        ghcr.io/%s:%s is pullable anonymously\n' "$GREEN" "$RESET" "$IMAGE_REPO" "$IMAGE_TAG"

# And it must be the image this repository records.
if [[ -f "$DIGEST_FILE" ]]; then
    recorded="$(tr -d '[:space:]' < "$DIGEST_FILE")"
    token="$(anon_token "$IMAGE_REPO" || true)"
    published="$(curl -sI --max-time 20 -H "Authorization: Bearer ${token}" -H "Accept: ${ACCEPT}" \
        "https://ghcr.io/v2/${IMAGE_REPO}/manifests/${IMAGE_TAG}" \
        | tr -d '\r' | awk -F': ' 'tolower($1)=="docker-content-digest"{print $2}')"
    if [[ "$published" != "$recorded" ]]; then
        printf '%sFAIL%s — the published digest is not the one recorded.\n' "$RED" "$RESET" >&2
        printf '       recorded:  %s\n       published: %s\n' "$recorded" "${published:-<none>}" >&2
        exit 1
    fi
    printf '%sok%s        published digest matches %s\n' "$GREEN" "$RESET" "$DIGEST_FILE"
fi

printf '\n%sPASS%s — the base image is published and obtainable without an account.\n' "$GREEN" "$RESET"
