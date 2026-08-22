//! What travels in a bundle, and what does not (M9, D-02, F-54).
//!
//! Exclusion is the compression strategy, not a tuning knob: dropping build
//! artifacts is worth roughly 95% on a real project, while codec choice is worth
//! roughly 20%. It is also the security boundary — D-02 says credentials are
//! per-device and never sync, and that is only true if this module makes it
//! true.
//!
//! Everything here is pure and host-free, so the decisions that matter can be
//! tested exhaustively without provisioning anything.

use std::collections::BTreeMap;

/// Why a path or field did not travel. Recorded in the manifest so the far side
/// can answer "where did my X go?" without guessing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExcludeReason {
    /// A secret. Never travels, not configurable (D-02).
    Secret,
    /// Identifies the source device or account.
    MachineSpecific,
    /// Regenerable cache.
    Cache,
    /// Build output, re-derivable from source.
    BuildArtifact,
    /// A field this build does not recognise. Stays put by default (F-54).
    UnrecognisedField,
    /// Another bundle sitting inside the exported tree. Bundles are export
    /// output, not project content: including one makes each export carry its
    /// predecessor, so a repeatedly-exported project grows without bound while
    /// every command reports success.
    NestedBundle,
}

impl ExcludeReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Secret => "secret",
            Self::MachineSpecific => "machine-specific",
            Self::Cache => "cache",
            Self::BuildArtifact => "build-artifact",
            Self::UnrecognisedField => "unrecognised-field",
            Self::NestedBundle => "nested-bundle",
        }
    }
}

/// Whether a member travels, and why not if it does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Include { class: Class },
    Exclude { reason: ExcludeReason },
}

/// Whether losing a member loses the session (from C1's measurements).
///
/// Carried into the manifest so import can materialise session-critical content
/// first and fetch the rest lazily — see `docs/bundle-format.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    SessionCritical,
    Reconstructible,
}

impl Class {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SessionCritical => "session-critical",
            Self::Reconstructible => "reconstructible",
        }
    }
}

/// Directories whose contents are build output or caches. Excluded by default;
/// a caller may opt back in (they are `overridable`).
const DEFAULT_EXCLUDED_DIRS: &[(&str, ExcludeReason)] = &[
    ("target", ExcludeReason::BuildArtifact),
    ("node_modules", ExcludeReason::BuildArtifact),
    ("__pycache__", ExcludeReason::BuildArtifact),
    (".venv", ExcludeReason::BuildArtifact),
    (".git/objects", ExcludeReason::Cache),
];

/// Extension of a written bundle. Files with this suffix are export output.
pub const BUNDLE_EXTENSION: &str = ".nemr";

/// Reconstructible Claude Code state, identified in C1.
///
/// **Volume-relative**, matching what an export actually walks. These were
/// originally the container-view paths (`root/.claude/backups`), which no export
/// ever sees — so the constant and its test agreed with each other and both
/// disagreed with reality, and the whole rule could be deleted with no
/// production change (F-58). M8 relocates session state under `.nemr-state/`,
/// so that is where reconstructible state appears if it ever does.
const RECONSTRUCTIBLE: &[&str] = &[".nemr-state/backups", ".nemr-state/.last-cleanup"];

/// The exclusion policy. Pluggable so the defaults can be relaxed per export
/// without the unconditional rules ever becoming negotiable.
#[derive(Debug, Clone, Default)]
pub struct Policy {
    /// Include build artifacts and caches that are excluded by default.
    ///
    /// Defaults to `false` — excluding them is the whole compression strategy.
    pub include_build_artifacts: bool,
}

impl Policy {
    /// Decide a single path, given relative to the export root.
    ///
    /// Paths use forward slashes and no leading slash: `workspace/notes.md`,
    /// `root/.claude/projects/-workspace/x.jsonl`.
    pub fn decide(&self, path: &str) -> Decision {
        // Unconditional first, so no later rule can accidentally re-include it.
        // D-02 is not a default; it is an invariant.
        //
        // Matched by FILE NAME anywhere in the tree, not by one absolute path.
        // Measured on a real export: the volume's layout is `.nemr-state/...`
        // plus project files at the root, while the container sees
        // `/root/.claude/...` — so a single hard-coded path matches nothing that
        // actually occurs, and a test asserting on it would pass while guarding
        // nothing. Today the credential is a host bind-mount and never reaches
        // the volume at all, which is what makes D-02 true; this check is the
        // belt to that braces, and it must survive a layout change.
        if is_credential_file(path) {
            return Decision::Exclude {
                reason: ExcludeReason::Secret,
            };
        }

        // `.claude.json` never travels whole — it is filtered per field instead
        // (see `filter_claude_json`), because it mixes portable config with
        // machine identity.
        if path == "root/.claude.json" {
            return Decision::Exclude {
                reason: ExcludeReason::MachineSpecific,
            };
        }

        // A bundle inside the exported tree never travels, regardless of policy.
        // This is not the same as the build-artifact defaults: those are a size
        // optimisation a caller may reasonably override, whereas nesting an
        // export inside an export is always a mistake and compounds with every
        // subsequent export.
        if path.ends_with(BUNDLE_EXTENSION) {
            return Decision::Exclude {
                reason: ExcludeReason::NestedBundle,
            };
        }

        if !self.include_build_artifacts {
            for (dir, reason) in DEFAULT_EXCLUDED_DIRS {
                if path_contains_dir(path, dir) {
                    return Decision::Exclude { reason: *reason };
                }
            }
        }

        for prefix in RECONSTRUCTIBLE {
            if path == *prefix || path.starts_with(&format!("{prefix}/")) {
                return Decision::Include {
                    class: Class::Reconstructible,
                };
            }
        }

        Decision::Include {
            class: Class::SessionCritical,
        }
    }
}

/// Whether `path` names the Claude Code credential file, wherever it sits.
///
/// Name-based rather than path-based deliberately: the export root's layout has
/// already differed from the container's once, and a secret filter that depends
/// on one layout is a filter that silently stops working when the layout moves.
pub fn is_credential_file(path: &str) -> bool {
    path.rsplit('/')
        .next()
        .is_some_and(|name| name == ".credentials.json")
}

/// Whether `path` has `dir` as one of its components (or a `a/b` component run).
fn path_contains_dir(path: &str, dir: &str) -> bool {
    if dir.contains('/') {
        return path == dir
            || path.starts_with(&format!("{dir}/"))
            || path.contains(&format!("/{dir}/"));
    }
    path.split('/').any(|component| component == dir)
}

// ---------------------------------------------------------------------------
// .claude.json — field-level allowlist (F-54)
// ---------------------------------------------------------------------------

/// Top-level `.claude.json` fields that travel.
///
/// An **allowlist**, deliberately. A blocklist would silently leak whatever
/// Anthropic adds in the next Claude Code release: we control neither that
/// schema nor our notice of it changing, so D-02 would hold only until the
/// schema moved. Defaulting new fields to *staying put* is the safe direction —
/// the cost of wrongly keeping a portable field is a missing convenience, while
/// the cost of wrongly sending an identity field is a leak.
const PORTABLE_FIELDS: &[&str] = &[
    // MCP server definitions — portable configuration a user wants on the far
    // side, and the reason this is a field-level split rather than dropping the
    // whole file.
    "mcpServers",
    // Per-project trust decisions, keyed by path.
    "projects",
];

/// What `filter_claude_json` decided, so the caller can log drift and record
/// exclusions in the manifest.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct FieldFilter {
    /// Fields that travel, in their original form.
    pub kept: BTreeMap<String, serde_json::Value>,
    /// Field names dropped because they are known to be machine- or
    /// account-shaped.
    pub dropped_known: Vec<String>,
    /// Field names dropped because this build does not recognise them. These
    /// are the ones worth logging: they are schema drift.
    pub dropped_unrecognised: Vec<String>,
}

/// Split a parsed `.claude.json` into what travels and what does not (F-54).
///
/// Unrecognised fields are dropped **and reported**, so schema drift surfaces as
/// a visible warning rather than a silent inclusion or a silent drop.
pub fn filter_claude_json(value: &serde_json::Value) -> FieldFilter {
    // Known machine/account-shaped fields. Listing them is not what provides the
    // safety — the allowlist above does that — but it lets the caller
    // distinguish "expected, dropped" from "new, dropped", and only the second
    // deserves a warning.
    // The full top-level key set measured on a real `.claude.json` in WP-C1,
    // minus the portable ones. Listing all of them is what keeps the drift
    // warning meaningful: if fields that already existed were reported as
    // unrecognised, every export would emit warnings, and a warning that always
    // fires is a warning nobody reads.
    const KNOWN_NON_PORTABLE: &[&str] = &[
        // Identity and account — the reason this is a field-level split.
        "machineID",
        "userID",
        "oauthAccount",
        // Install/setup state, specific to this machine.
        "installMethod",
        "autoUpdates",
        "firstStartTime",
        "migrationVersion",
        "seenNotifications",
        // Migration and rollout flags observed in C1. Machine-local state about
        // what this install has already done.
        "opusProMigrationComplete",
        "sonnet1m45MigrationComplete",
        "hasResetAutoModeOptInForDefaultOffer",
        // Caches not caught by the prefix/suffix heuristics below.
        "clientDataCacheSlots",
    ];

    let mut filter = FieldFilter::default();
    let Some(object) = value.as_object() else {
        return filter;
    };

    for (key, field) in object {
        if PORTABLE_FIELDS.contains(&key.as_str()) {
            filter.kept.insert(key.clone(), field.clone());
        } else if KNOWN_NON_PORTABLE.contains(&key.as_str())
            || key.starts_with("cached")
            || key.ends_with("Cache")
        {
            filter.dropped_known.push(key.clone());
        } else {
            filter.dropped_unrecognised.push(key.clone());
        }
    }
    filter
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The credential filter must work on the layout that actually occurs.
    ///
    /// Measured from a real export: the volume holds `.nemr-state/projects/...`
    /// and project files at the root — not the container's `/root/.claude/...`.
    /// The original absolute-path check matched none of these, so its test
    /// passed while guarding a path that cannot exist.
    #[test]
    fn credentials_are_caught_in_the_layout_that_really_occurs() {
        // Every policy, not just the default: D-02 is an invariant, not a
        // setting. (Absorbs the deleted `credentials_never_travel_under_any_policy`,
        // which asserted on a container-view path that cannot occur and against a
        // `CREDENTIAL_PATH` constant production no longer consulted — F-58.)
        for policy in [
            Policy::default(),
            Policy {
                include_build_artifacts: true,
            },
        ] {
            for path in [
                // container-view (what the first implementation assumed)
                "root/.claude/.credentials.json",
                // volume-view variants, which is what an export actually walks
                ".credentials.json",
                ".nemr-state/.credentials.json",
                ".nemr-state/projects/.credentials.json",
                "some/deeply/nested/.credentials.json",
            ] {
                assert_eq!(
                    policy.decide(path),
                    Decision::Exclude {
                        reason: ExcludeReason::Secret
                    },
                    "{path} must be refused wherever it sits, under any policy (D-02)"
                );
            }
        }
    }

    /// Real member paths from a measured export must classify sensibly.
    #[test]
    fn the_real_export_layout_classifies_correctly() {
        let policy = Policy::default();
        // Captured from an actual bundle manifest.
        assert_eq!(
            policy.decide(".nemr-state/projects/-workspace/a7f85305.jsonl"),
            Decision::Include {
                class: Class::SessionCritical
            },
            "the transcript is the session"
        );
        assert_eq!(
            policy.decide("recipe.md"),
            Decision::Include {
                class: Class::SessionCritical
            },
            "project files at the volume root travel"
        );
        // Build artifacts sit at the volume root in reality, not under workspace/.
        assert!(
            matches!(policy.decide("target/debug/app"), Decision::Exclude { .. }),
            "component matching still catches build output at the root"
        );
    }

    /// Reconstructible state is classified from the layout an export really
    /// walks, not the container's view.
    ///
    /// The prefixes were previously container-view (`root/.claude/backups`),
    /// which no export encounters — test and constant agreed with each other and
    /// both disagreed with reality (F-58).
    #[test]
    fn caches_and_housekeeping_are_reconstructible() {
        let policy = Policy::default();
        for path in [".nemr-state/backups/x.json", ".nemr-state/.last-cleanup"] {
            assert_eq!(
                policy.decide(path),
                Decision::Include {
                    class: Class::Reconstructible
                },
                "{path} is regenerable and must not be session-critical"
            );
        }
        // Control: session state under the same prefix must stay critical, or
        // the rule would be over-broad.
        assert_eq!(
            policy.decide(".nemr-state/projects/-workspace/a.jsonl"),
            Decision::Include {
                class: Class::SessionCritical
            },
            "the transcript must remain session-critical"
        );
    }

    #[test]
    fn build_artifacts_are_excluded_by_default_and_overridable() {
        let strict = Policy::default();
        let permissive = Policy {
            include_build_artifacts: true,
        };
        for path in [
            "workspace/target/debug/app",
            "workspace/node_modules/left-pad/index.js",
            "workspace/sub/__pycache__/m.pyc",
        ] {
            assert!(
                matches!(strict.decide(path), Decision::Exclude { .. }),
                "{path} is excluded by default (this is the 95%)"
            );
            assert!(
                matches!(permissive.decide(path), Decision::Include { .. }),
                "{path} is included when the caller opts in"
            );
        }
    }

    /// A directory name must match as a path *component*, not a substring, or
    /// `my-target-notes.md` would be silently dropped.
    #[test]
    fn directory_exclusions_match_components_not_substrings() {
        let policy = Policy::default();
        assert!(
            matches!(
                policy.decide("workspace/my-target-notes.md"),
                Decision::Include { .. }
            ),
            "a file whose name merely contains 'target' must travel"
        );
        assert!(
            matches!(
                policy.decide("workspace/targets/list.txt"),
                Decision::Include { .. }
            ),
            "'targets' is not 'target'"
        );
    }

    /// Bundles are export output, not project content. Including one makes each
    /// export carry its predecessor — unbounded growth, reported as success.
    #[test]
    fn a_bundle_inside_the_tree_never_travels() {
        for policy in [
            Policy::default(),
            Policy {
                include_build_artifacts: true,
            },
        ] {
            assert_eq!(
                policy.decide("workspace/backup.nemr"),
                Decision::Exclude {
                    reason: ExcludeReason::NestedBundle
                },
                "a nested bundle must never travel, under any policy"
            );
        }
    }

    // --- F-54: the allowlist ------------------------------------------------

    #[test]
    fn claude_json_never_travels_whole() {
        assert_eq!(
            Policy::default().decide("root/.claude.json"),
            Decision::Exclude {
                reason: ExcludeReason::MachineSpecific
            },
            "the file is filtered per field, never copied wholesale"
        );
    }

    #[test]
    fn mcp_configuration_travels_and_identity_does_not() {
        let config = json!({
            "mcpServers": { "gh": { "command": "gh-mcp" } },
            "projects": { "/workspace": { "trusted": true } },
            "machineID": "abc123",
            "userID": "u-1",
            "oauthAccount": { "emailAddress": "someone@example.com" },
            "cachedGrowthBookFeatures": { "x": 1 }
        });
        let filter = filter_claude_json(&config);

        assert!(filter.kept.contains_key("mcpServers"), "MCP config travels");
        assert!(
            filter.kept.contains_key("projects"),
            "project trust travels"
        );
        for identity in ["machineID", "userID", "oauthAccount"] {
            assert!(
                !filter.kept.contains_key(identity),
                "{identity} must not travel"
            );
            assert!(
                filter.dropped_known.contains(&identity.to_string()),
                "{identity} is dropped as known, not flagged as drift"
            );
        }
        assert!(
            filter
                .dropped_known
                .contains(&"cachedGrowthBookFeatures".to_string()),
            "cache blocks are dropped as known"
        );
        assert!(
            filter.dropped_unrecognised.is_empty(),
            "nothing here is unrecognised: {:?}",
            filter.dropped_unrecognised
        );
    }

    /// The point of the allowlist. A field nobody has seen before must stay put
    /// and be reported — this is the test that fails if someone "helpfully"
    /// converts this to a blocklist.
    #[test]
    fn unknown_fields_stay_put_and_are_reported() {
        let config = json!({
            "mcpServers": {},
            "somethingAnthropicAddedLastTuesday": { "secretish": "value" }
        });
        let filter = filter_claude_json(&config);

        assert!(
            !filter
                .kept
                .contains_key("somethingAnthropicAddedLastTuesday"),
            "an unrecognised field must NOT travel — a blocklist would have leaked it"
        );
        assert_eq!(
            filter.dropped_unrecognised,
            vec!["somethingAnthropicAddedLastTuesday".to_string()],
            "and it must be reported, so schema drift is visible rather than silent"
        );
    }

    /// The drift canary, pinned to reality.
    ///
    /// These are the exact top-level keys measured on a real `.claude.json` in
    /// WP-C1. Every one must classify as *known* — kept or deliberately
    /// dropped — so that an "unrecognised field" warning means the schema
    /// genuinely moved rather than that we never finished the list. Testing the
    /// allowlist only against synthetic input hid four such fields until this
    /// was run against the measured key set.
    #[test]
    fn the_real_measured_key_set_produces_no_drift_warnings() {
        const MEASURED_KEYS: &[&str] = &[
            "installMethod",
            "autoUpdates",
            "cachedGrowthBookFeatures",
            "firstStartTime",
            "machineID",
            "opusProMigrationComplete",
            "sonnet1m45MigrationComplete",
            "seenNotifications",
            "hasResetAutoModeOptInForDefaultOffer",
            "migrationVersion",
            "userID",
            "oauthAccount",
            "cachedExperimentFeatures",
            "cachedExperimentData",
            "cachedGrowthBookFeaturesAt",
            "clientDataCacheSlots",
            "additionalModelOptionsCache",
            "additionalModelCostsCache",
            "modelAccessCache",
            "orgModelDefaultCache",
            "autoCompactWindowsCache",
            "cachedExtraUsageDisabledReason",
            "groveConfigCache",
            "passesEligibilityCache",
            "mcpServers",
        ];
        let mut object = serde_json::Map::new();
        for key in MEASURED_KEYS {
            object.insert((*key).to_string(), json!("x"));
        }
        let filter = filter_claude_json(&serde_json::Value::Object(object));

        assert!(
            filter.dropped_unrecognised.is_empty(),
            "fields that existed when we measured must not be reported as drift, or every \
             export warns and nobody reads the warning. Unclassified: {:?}",
            filter.dropped_unrecognised
        );
        assert_eq!(
            filter.kept.keys().collect::<Vec<_>>(),
            vec!["mcpServers"],
            "of the real key set, only MCP configuration travels"
        );
        assert!(
            filter.dropped_known.len() >= 20,
            "the rest are dropped as known, not silently kept"
        );
    }

    #[test]
    fn a_non_object_config_yields_nothing_rather_than_panicking() {
        assert_eq!(
            filter_claude_json(&json!("not an object")),
            FieldFilter::default()
        );
        assert_eq!(filter_claude_json(&json!(null)), FieldFilter::default());
    }
}
