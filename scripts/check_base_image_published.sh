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

status="$(manifest_status "$IMAGE_REPO" "$IMAGE_TAG")"
if [[ "$status" != "200" ]]; then
    cat >&2 <<EOF

${RED}FAIL${RESET} — ghcr.io/${IMAGE_REPO}:${IMAGE_TAG} is not pullable without an account (HTTP ${status}).

  The push succeeded and the digest is recorded, so nothing else reports this.
  But D-08 chose GHCR because the image would be obtainable without an account,
  and E-11 depends on the open half being usable without paying. A private
  package satisfies neither.

  GHCR packages published with GITHUB_TOKEN default to private. Fix once, by
  hand — a workflow cannot set its own package visibility:

    https://github.com/users/gnrain/packages/container/nemr-base/settings
    -> Danger Zone -> Change visibility -> Public

  Then re-run this check.

EOF
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
