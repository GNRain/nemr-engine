#!/usr/bin/env bash
#
# The server's settings, for scripts — the same file, the same rules.
#
# The Product Owner's rule (2026-09-11): *"anything that needs me to export
# something first either reads the file or refuses naming the file and the
# key."* Every acceptance that needs a database or a bucket used to need a
# `source ~/.config/nemr/r2.env` and an `export DATABASE_URL=` first. Now they
# read `sync.env`, which is where `nemr server configure` puts everything.
#
#   . scripts/lib/settings.sh
#   nemr_settings_load                       # the file's keys, where unset
#   nemr_settings_require DATABASE_URL       # or refuse, naming both
#
# THE RULES, matching crates/nemr-sync/src/settings.rs exactly:
#   - the file is $NEMR_SYNC_ENV_FILE, else $XDG_CONFIG_HOME/nemr/sync.env,
#     else ~/.config/nemr/sync.env; an EMPTY NEMR_SYNC_ENV_FILE means read none
#   - KEY=VALUE lines, `export ` tolerated, blank lines and # comments skipped,
#     one pair of surrounding quotes stripped
#   - THE PROCESS ENVIRONMENT WINS, key by key
#   - a file wider than 0600 is refused, not repaired — it holds the pepper and
#     the storage credential
#
# A value read here is never echoed. These are secrets.

[[ -n "${_NEMR_SETTINGS_SH_LOADED:-}" ]] && return 0 2>/dev/null || true
_NEMR_SETTINGS_SH_LOADED=1

nemr_settings_file() {
    if [[ -n "${NEMR_SYNC_ENV_FILE+x}" ]]; then
        printf '%s' "$NEMR_SYNC_ENV_FILE"      # empty means: read no file
        return 0
    fi
    if [[ -n "${XDG_CONFIG_HOME:-}" ]]; then
        printf '%s' "$XDG_CONFIG_HOME/nemr/sync.env"
        return 0
    fi
    printf '%s' "${HOME:-}/.config/nemr/sync.env"
}

# Export what the file carries and the environment does not. Silent about
# values, and about a file that is not there — a caller with everything already
# exported is a caller with nothing to do here.
nemr_settings_load() {
    local file mode key value line
    file="$(nemr_settings_file)"
    [[ -n "$file" && -r "$file" ]] || return 0
    mode="$(stat -c '%a' "$file" 2>/dev/null || echo 600)"
    if [[ "$mode" != "600" && "$mode" != "400" && "$mode" != "0600" && "$mode" != "0400" ]]; then
        printf 'settings: %s is mode %s; it holds the pepper and the storage credential.\n' \
            "$file" "$mode" >&2
        printf '          chmod 600 %s\n' "$file" >&2
        return 1
    fi
    while IFS= read -r line || [[ -n "$line" ]]; do
        line="${line#"${line%%[![:space:]]*}"}"          # ltrim
        [[ -z "$line" || "${line:0:1}" == "#" ]] && continue
        line="${line#export }"
        [[ "$line" == *=* ]] || continue
        key="${line%%=*}"; value="${line#*=}"
        key="${key%"${key##*[![:space:]]}"}"             # rtrim the key
        [[ "$key" =~ ^[A-Za-z_][A-Za-z_0-9]*$ ]] || continue
        value="${value#"${value%%[![:space:]]*}"}"
        value="${value%"${value##*[![:space:]]}"}"
        # One pair of surrounding quotes, as a shell would strip.
        if [[ "${value:0:1}" == '"' && "${value: -1}" == '"' && ${#value} -ge 2 ]]; then
            value="${value:1:${#value}-2}"
        elif [[ "${value:0:1}" == "'" && "${value: -1}" == "'" && ${#value} -ge 2 ]]; then
            value="${value:1:${#value}-2}"
        fi
        # The environment wins, key by key.
        [[ -n "${!key+x}" && -n "${!key}" ]] && continue
        export "$key=$value"
    done < "$file"
    return 0
}

# Refuse, naming the file AND the key — the two things the reader needs, and
# neither is guessable. Never prints a value.
nemr_settings_require() {
    local missing=() key file
    file="$(nemr_settings_file)"
    for key in "$@"; do
        [[ -n "${!key:-}" ]] || missing+=("$key")
    done
    (( ${#missing[@]} == 0 )) && return 0
    printf '\n' >&2
    printf 'This needs %s, and neither the environment nor the settings file has it.\n' \
        "$(printf '%s, ' "${missing[@]}" | sed 's/, $//')" >&2
    printf '\n' >&2
    if [[ -z "$file" ]]; then
        printf '  NEMR_SYNC_ENV_FILE is empty, which means "read no settings file".\n' >&2
        printf '  Unset it, or export what is missing for this command.\n' >&2
    else
        printf '  The file it reads:  %s\n' "$file" >&2
        if [[ -e "$file" ]]; then
            printf '  It exists; add the missing line to it (mode 0600).\n' >&2
        else
            printf '  It does not exist yet.  nemr server configure  writes it.\n' >&2
        fi
    fi
    printf '\n' >&2
    return 1
}
