#!/usr/bin/env bash
#
# THE one place shell learns the base image (F-124).
#
# `src/config.rs`'s BASE_IMAGE constant is the source of truth; every script
# that needs the image or its version reads it through here. Before this,
# five files hardcoded the version independently, a bump was a six-file change
# whose pieces could disagree — config.rs and build_base_image.sh disagreeing
# means the host suite builds one image and tests another — and the 0.3.0 bump
# needed a hand-written ordering to stay safe. Now a bump edits config.rs and
# everything moves.
#
# Source it for the functions, or execute it to print the image (which is how
# the diagnostic probe binaries are pointed at it without the containerd crate
# growing a dependency on the engine):
#
#   . "$(dirname "${BASH_SOURCE[0]}")/lib/base_image.sh"
#   IMAGE="${NEMR_BASE_IMAGE:-$(nemr_base_image)}"
#
#   NEMR_PROBE_IMAGE="$(scripts/lib/base_image.sh)" cargo run --bin gc_probe
#
# The extraction CONTROLS itself: a grep that matches nothing, or matches
# something that is not shaped like an image reference, is a loud failure —
# never an empty string. An empty image name does not error downstream; it
# builds or queries a wrongly-named artifact, which is the silent-wrong-result
# shape this project keeps finding.

# Guard against double-sourcing.
[[ -n "${_NEMR_BASE_IMAGE_SH_LOADED:-}" ]] && return 0 2>/dev/null || true
_NEMR_BASE_IMAGE_SH_LOADED=1

# The repo root, relative to THIS FILE — not to the caller's cwd, which is
# whatever it is.
_nemr_repo_root() {
    cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd
}

# Print the full base image reference (registry/name:version, from config.rs).
# No example version here on purpose: the drift guard scans this file too, and
# a version in prose goes stale at the next bump with nothing to catch it.
nemr_base_image() {
    local config root image
    root="$(_nemr_repo_root)" || return 1
    config="$root/src/config.rs"
    if [[ ! -f "$config" ]]; then
        echo "base_image.sh: $config does not exist — cannot determine the base image" >&2
        return 1
    fi
    image="$(sed -n 's/^pub const BASE_IMAGE: &str = "\(.*\)";$/\1/p' "$config" | head -1)"
    if [[ ! "$image" =~ ^[a-z0-9.-]+/[a-z0-9./_-]+:[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
        echo "base_image.sh: could not extract a well-formed image reference from" >&2
        echo "  $config (got: '${image:-<empty>}')" >&2
        echo "The constant's shape changed; update the extraction here IN THE SAME CHANGE." >&2
        return 1
    fi
    printf '%s\n' "$image"
}

# Print just the version tag, e.g. 0.3.0.
nemr_base_version() {
    local image
    image="$(nemr_base_image)" || return 1
    printf '%s\n' "${image##*:}"
}

# Executed directly (not sourced): print the image, for callers outside bash.
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
    nemr_base_image
fi
