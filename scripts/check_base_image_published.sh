#!/usr/bin/env bash
#
# EVERY published base image version must stay pullable WITHOUT an account, at
# the exact bytes the repository records (D-08, E-11, F-85).
#
# Publishing is not the goal; being obtainable is. D-08 chose GHCR on the
# argument that the image is "obtainable without an account", and E-11's test —
# can someone use the open half productively without ever paying? — depends on
# it. F-85 adds the second half: a version tag names one set of bytes forever,
# and every version stays published, because a bundle records the digest it was
# built from and can be restored only where that exact image is available.
#
# So this checks the WHOLE ledger, not just the current version (F-87). If an
# old version became unreachable, or its published digest drifted from the
# recorded one, an old bundle silently stops being importable — the precise
# guarantee F-85 exists to provide, which was asserted rather than enforced
# until this checked every version.
#
#   ./scripts/check_base_image_published.sh                  # every version
#   NEMR_BASE_VERSION=0.1.0 ./scripts/check_base_image_published.sh   # just one
#
# Anonymous by construction: it deliberately sends no credential, because "it
# works for me" is exactly the failure mode here.

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

IMAGE_REPO="${NEMR_BASE_REPO:-gnrain/nemr-base}"
ONLY_VERSION="${NEMR_BASE_VERSION:-}"   # optional: restrict to one version
DIGESTS_DIR="image/digests"
ACCEPT='application/vnd.oci.image.manifest.v1+json,application/vnd.oci.image.index.v1+json,application/vnd.docker.distribution.manifest.v2+json,application/vnd.docker.distribution.manifest.list.v2+json'

RED=$'\033[31m'; GREEN=$'\033[32m'; RESET=$'\033[0m'
ok()  { printf '%sok%s        %s\n' "$GREEN" "$RESET" "$1"; }

anon_token() {
    curl -fsS --max-time 20 "https://ghcr.io/token?service=ghcr.io&scope=repository:$1:pull" \
        | python3 -c 'import sys,json;print(json.load(sys.stdin).get("token",""))'
}

# One request per version: return "<http_status> <content_digest>" so
# reachability and the digest are read together.
manifest_probe() {
    local repo="$1" ref="$2" token headers
    token="$(anon_token "$repo" || true)"
    headers="$(curl -sI --max-time 20 \
        -H "Authorization: Bearer ${token}" -H "Accept: ${ACCEPT}" \
        "https://ghcr.io/v2/${repo}/manifests/${ref}" || true)"
    local status digest
    status="$(printf '%s' "$headers" | tr -d '\r' | awk 'NR==1{print $2}')"
    digest="$(printf '%s' "$headers" | tr -d '\r' \
        | awk -F': ' 'tolower($1)=="docker-content-digest"{print $2}')"
    printf '%s %s\n' "${status:-000}" "${digest:-<none>}"
}

# ---------------------------------------------------------------------------
# CONTROL first: an unauthenticated client must be able to read a package that
# is genuinely public. Without this, a 403/404 below could mean "GHCR refuses
# everyone anonymously" or "the network is down here" rather than a real
# per-version problem, and the remedy would be wrong. This is the control that
# made a purpose-built check beat plausible one-liners (F-87 note).
# ---------------------------------------------------------------------------
read -r control_status _ < <(manifest_probe "actions/actions-runner" "latest") || true
if [[ "$control_status" != "200" ]]; then
    printf '%sFAIL%s — control: a known-public GHCR package answered HTTP %s anonymously.\n' \
        "$RED" "$RESET" "$control_status" >&2
    printf '       This check cannot distinguish a real per-version problem from "anonymous\n' >&2
    printf '       access to GHCR is not working from here", so it is not reporting.\n' >&2
    exit 1
fi
ok "control: a known-public GHCR package is readable anonymously (HTTP 200)"

# ---------------------------------------------------------------------------
# Gather the versions to check from the ledger itself, so a new version is
# covered automatically the moment its digest file is added.
# ---------------------------------------------------------------------------
versions=()
for f in "$DIGESTS_DIR"/*; do
    base="$(basename "$f")"
    [[ "$base" == "README.md" ]] && continue
    [[ -n "$ONLY_VERSION" && "$base" != "$ONLY_VERSION" ]] && continue
    versions+=("$base")
done

# CONTROL: there must be something to check, or a clean pass is meaningless.
if [[ ${#versions[@]} -eq 0 ]]; then
    printf '%sFAIL%s — no versions to check under %s%s.\n' "$RED" "$RESET" "$DIGESTS_DIR" \
        "${ONLY_VERSION:+ matching NEMR_BASE_VERSION=$ONLY_VERSION}" >&2
    exit 1
fi

check_version() {
    local version="$1"
    local recorded probe status published
    recorded="$(tr -d '[:space:]' < "${DIGESTS_DIR}/${version}")"

    # Test seams (F-86/F-87): force the status and/or digest so the branches are
    # provable without a live registry. The control above still runs against the
    # real network, so a forced run cannot pass vacuously.
    if [[ -n "${NEMR_TEST_FORCE_STATUS:-}" ]]; then
        status="$NEMR_TEST_FORCE_STATUS"
        published="${NEMR_TEST_FORCE_DIGEST:-$recorded}"
    else
        read -r status published < <(manifest_probe "$IMAGE_REPO" "$version") || true
    fi

    if [[ "$status" != "200" ]]; then
        case "$status" in
            403) printf '%sFAIL%s — ghcr.io/%s:%s exists but is PRIVATE (HTTP 403).\n' \
                    "$RED" "$RESET" "$IMAGE_REPO" "$version" >&2
                 printf '            A workflow cannot set its own package visibility. Fix once:\n' >&2
                 printf '            https://github.com/users/gnrain/packages/container/nemr-base/settings\n' >&2
                 printf '            -> Danger Zone -> Change visibility -> Public\n' >&2 ;;
            404) printf '%sFAIL%s — ghcr.io/%s:%s is NOT PUBLISHED (HTTP 404).\n' \
                    "$RED" "$RESET" "$IMAGE_REPO" "$version" >&2
                 printf '            The registry has no such version. A version recorded in the\n' >&2
                 printf '            ledger but missing from the registry breaks every bundle built\n' >&2
                 printf '            against it. Publish it, or if it was never meant to exist,\n' >&2
                 printf '            remove image/digests/%s.\n' "$version" >&2 ;;
            *)   printf '%sFAIL%s — ghcr.io/%s:%s is not readable anonymously (HTTP %s).\n' \
                    "$RED" "$RESET" "$IMAGE_REPO" "$version" "$status" >&2
                 printf '            Unexpected; the control passed, so inspect this version by hand.\n' >&2 ;;
        esac
        return 1
    fi

    # Reachable — now it must be the EXACT bytes recorded. A drift here means the
    # tag was moved to different content, which orphans every bundle that named
    # the old digest (F-85). This is the immutability guarantee, enforced.
    if [[ "$published" != "$recorded" ]]; then
        printf '%sFAIL%s — ghcr.io/%s:%s digest DRIFTED from the recorded bytes (F-85).\n' \
            "$RED" "$RESET" "$IMAGE_REPO" "$version" >&2
        printf '            recorded (image/digests/%s): %s\n' "$version" "$recorded" >&2
        printf '            served by the registry now:   %s\n' "$published" >&2
        printf '            A version tag must never move. Every bundle built against %s is now\n' "$version" >&2
        printf '            unrecoverable. Restore the original bytes at this tag.\n' >&2
        return 1
    fi

    ok "$version: pullable anonymously, digest matches image/digests/$version"
    return 0
}

failures=0
for version in "${versions[@]}"; do
    check_version "$version" || failures=$((failures + 1))
done

echo
if [[ "$failures" -ne 0 ]]; then
    printf '%sFAIL%s — %d of %d version(s) unreachable or drifted. Old bundles depend on these.\n' \
        "$RED" "$RESET" "$failures" "${#versions[@]}" >&2
    exit 1
fi
printf '%sPASS%s — all %d published version(s) reachable at the recorded bytes.\n' \
    "$GREEN" "$RESET" "${#versions[@]}"
