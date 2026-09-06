//! Seeding a session's Claude Code config so the first `claude` opens ready
//! (F-131, extended to identity and onboarding; measured 2026-09-06).
//!
//! The feature, as the user experiences it: they logged in on the host once;
//! `nemr create`, `start`, `attach`, `claude` — and they are already
//! authenticated, no theme picker, no login method, no browser. What actually
//! happened before this: a fresh session's `.claude.json` is absent, so Claude
//! Code ran first-run onboarding — the theme picker, then "Select login method"
//! — with a perfectly valid token mounted beside it.
//!
//! What the measurements showed (docs/first-run-acceptance.sh reproduces them):
//!
//! | seeded into `.claude.json`                  | theme picker | login prompt | trust dialog | ready |
//! |---------------------------------------------|--------------|--------------|--------------|-------|
//! | nothing                                     | yes          | yes          | —            | no    |
//! | identity (`oauthAccount`) only              | yes          | yes          | —            | no    |
//! | onboarding flags only                       | no           | no           | yes          | no    |
//! | onboarding flags + `/workspace` trusted     | no           | no           | no           | **yes** |
//!
//! So the login prompt is a step of *onboarding*, not a consequence of the
//! missing identity: the two onboarding flags remove both prompts, the trust
//! entry removes the last dialog, and Claude Code populates `oauthAccount`
//! itself from the token on its first run (eighteen fields, measured). The
//! seed still copies the host's identity block when present — the same
//! account the token belongs to, so nothing is invented — because it is
//! harmless and the session shows the right account before its first request.
//!
//! **An allowlist, deliberately** (F-54's rule): what travels is named here
//! by field; everything else in the host's `.claude.json` — `machineID`,
//! `userID`, the host's own `projects` map keyed by host paths, sixty-odd
//! caches — stays where it is. The `/workspace` trust entry is *fresh*, not
//! copied: Nemr asserts that the session's own volume is trusted, because the
//! user created it; nothing about the host's projects crosses.

use serde_json::{json, Map, Value};

/// The container path Claude Code opens the session in: the volume.
pub const WORKSPACE: &str = "/workspace";

/// Host fields copied verbatim when present. Onboarding state and, when the
/// host has it, a display preference and the identity block.
pub const COPIED_FIELDS: &[&str] = &[
    "hasCompletedOnboarding",
    "lastOnboardingVersion",
    "theme",
    "oauthAccount",
];

/// Build the session's initial `.claude.json` from the host's, or from nothing.
///
/// Total: a missing or unparseable host file still yields a seed that opens
/// ready — onboarding is marked complete regardless (the user has a credential
/// on this host, which is the login onboarding exists to obtain), and the
/// workspace is trusted. Identity is copied only if the host has it; Claude
/// Code fills it from the token otherwise.
pub fn session_config_seed(host: Option<&Value>) -> Value {
    let mut out = Map::new();
    if let Some(Value::Object(h)) = host {
        for key in COPIED_FIELDS {
            if let Some(v) = h.get(*key) {
                out.insert((*key).to_string(), v.clone());
            }
        }
    }
    // Onboarding is complete by definition: a credential exists on this host.
    out.insert("hasCompletedOnboarding".into(), Value::Bool(true));
    // The session's own volume is trusted; the user created it.
    out.insert(
        "projects".into(),
        json!({ WORKSPACE: { "hasTrustDialogAccepted": true } }),
    );
    Value::Object(out)
}

/// The shell command that writes the seed inside the session, once: only if
/// no `.claude.json` exists yet. The JSON travels as `$0`, an argument, so no
/// quoting of its contents is involved. Idempotent by construction — a second
/// start, or a session that has already run `claude`, is left alone.
pub fn seed_write_argv(seed: &Value) -> Vec<String> {
    vec![
        "/bin/sh".into(),
        "-c".into(),
        "test -e /root/.claude.json || printf '%s' \"$0\" > /root/.claude.json".into(),
        seed.to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host() -> Value {
        json!({
            "hasCompletedOnboarding": true,
            "lastOnboardingVersion": "2.1.261",
            "oauthAccount": { "accountUuid": "acc-1", "emailAddress": "u@example.com" },
            "machineID": "host-machine",
            "userID": "host-user",
            "projects": { "/home/u/somewhere": { "hasTrustDialogAccepted": true, "allowedTools": ["Bash"] } },
            "cachedGrowthBookFeatures": { "x": 1 },
            "mcpServers": { "s": {} }
        })
    }

    /// The allowlist: exactly the named fields cross, nothing else.
    #[test]
    fn the_seed_copies_only_the_allowlisted_fields() {
        let seed = session_config_seed(Some(&host()));
        let keys: Vec<&str> = seed
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        for k in [
            "hasCompletedOnboarding",
            "lastOnboardingVersion",
            "oauthAccount",
            "projects",
        ] {
            assert!(keys.contains(&k), "{k} must be seeded: {keys:?}");
        }
        for k in [
            "machineID",
            "userID",
            "cachedGrowthBookFeatures",
            "mcpServers",
        ] {
            assert!(!keys.contains(&k), "{k} must NOT cross: {keys:?}");
        }
        assert_eq!(seed["oauthAccount"]["emailAddress"], "u@example.com");
    }

    /// The host's own projects map never crosses; the session gets a fresh
    /// trust entry for its own workspace and nothing else under `projects`.
    #[test]
    fn the_workspace_is_trusted_and_the_hosts_projects_map_does_not_cross() {
        let seed = session_config_seed(Some(&host()));
        let projects = seed["projects"].as_object().unwrap();
        assert_eq!(projects.len(), 1, "{projects:?}");
        assert_eq!(seed["projects"][WORKSPACE]["hasTrustDialogAccepted"], true);
        assert!(seed["projects"].get("/home/u/somewhere").is_none());
    }

    /// No host file, or a host that never onboarded, still opens ready: the
    /// flags are asserted, identity is left for Claude Code to fill.
    #[test]
    fn without_a_host_config_the_seed_still_opens_ready() {
        for host in [None, Some(json!({})), Some(json!("not an object"))] {
            let seed = session_config_seed(host.as_ref());
            assert_eq!(seed["hasCompletedOnboarding"], true);
            assert_eq!(seed["projects"][WORKSPACE]["hasTrustDialogAccepted"], true);
            assert!(seed.get("oauthAccount").is_none());
        }
        // A host that explicitly has it false is still seeded true: the
        // credential's presence is the onboarding.
        let seed = session_config_seed(Some(&json!({ "hasCompletedOnboarding": false })));
        assert_eq!(seed["hasCompletedOnboarding"], true);
    }

    /// The write is guarded by `test -e`, and the JSON is an argument, so a
    /// seed containing shell-significant characters cannot break the command.
    #[test]
    fn the_write_command_is_idempotent_and_passes_the_json_as_an_argument() {
        let seed = json!({ "oauthAccount": { "displayName": "O'Brien \"$(x)\" `y`" } });
        let argv = seed_write_argv(&seed);
        assert_eq!(argv[0], "/bin/sh");
        assert!(
            argv[2].starts_with("test -e /root/.claude.json ||"),
            "{}",
            argv[2]
        );
        assert!(
            !argv[2].contains("O'Brien"),
            "the JSON must not be interpolated into the script"
        );
        assert_eq!(argv[3], seed.to_string());
        assert_eq!(serde_json::from_str::<Value>(&argv[3]).unwrap(), seed);
    }
}
