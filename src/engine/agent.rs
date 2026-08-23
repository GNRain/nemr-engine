//! The coding agent a project runs.
//!
//! Nemr does not assume Claude Code, but it does assume **one agent at a time**
//! per project (E-15). Each agent's CLI stores its conversation in its own
//! undocumented format; two agents writing side by side would give a project two
//! disconnected pasts with no way to tell which is real. The project records
//! which one it uses, and — separately — the bundle records which one *produced*
//! a session, leaving room for cross-agent migration later without a schema
//! change.

use std::fmt;
use std::str::FromStr;

/// A coding agent Nemr can run inside a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    /// Anthropic's Claude Code. The default, and the only agent before E-15.
    ClaudeCode,
    /// OpenAI's Codex CLI.
    Codex,
}

impl Agent {
    /// Every known agent, in menu order (the default first).
    pub fn all() -> &'static [Agent] {
        &[Agent::ClaudeCode, Agent::Codex]
    }

    /// The default for a new project, and for any project created before the
    /// agent field existed — those are Claude Code by construction (E-15).
    pub fn default_agent() -> Agent {
        Agent::ClaudeCode
    }

    /// The stable identifier stored in labels and manifests. Never change these
    /// — a stored project or bundle refers to an agent by this string.
    pub fn id(self) -> &'static str {
        match self {
            Agent::ClaudeCode => "claude-code",
            Agent::Codex => "codex",
        }
    }

    /// The command run inside the container to launch the agent.
    pub fn command(self) -> &'static str {
        match self {
            Agent::ClaudeCode => "claude",
            Agent::Codex => "codex",
        }
    }

    /// A one-line description, shown under each option at the interactive
    /// prompt so someone who has never used an agent knows what they are
    /// choosing.
    pub fn description(self) -> &'static str {
        match self {
            Agent::ClaudeCode => "Anthropic's Claude Code (default)",
            Agent::Codex => "OpenAI's Codex CLI (implemented but unverified — see F-84)",
        }
    }

    /// Whether Nemr's session portability has been empirically verified for
    /// this agent (E-15).
    ///
    /// Claude Code was verified by WP-C: its session-state layout was captured
    /// from a real session, the session-critical parts were relocated onto the
    /// portable volume (M8), and portability was proven end to end (M10). Codex
    /// is IMPLEMENTED the same way but NOT verified — where it writes session
    /// state has never been measured, so the relocation may point at the wrong
    /// paths, and D-02 (the credential never travels) is asserted for it, not
    /// enforced. Verifying it needs a real Codex session with API round-trips,
    /// which needs Codex authenticated on the host (see F-84).
    ///
    /// This is a method, not a doc note, so the CLI can warn at the point of
    /// selection rather than trusting the user to read the docs.
    pub fn portability_verified(self) -> bool {
        match self {
            Agent::ClaudeCode => true,
            Agent::Codex => false,
        }
    }

    /// A human-facing label for menus and status output.
    pub fn label(self) -> &'static str {
        match self {
            Agent::ClaudeCode => "Claude Code",
            Agent::Codex => "Codex",
        }
    }
}

impl fmt::Display for Agent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

impl FromStr for Agent {
    type Err = crate::error::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let normalised = s.trim().to_ascii_lowercase();
        // Accept the stored id and a couple of forgiving aliases, but nothing
        // vague: an unknown agent must fail loudly, never silently become the
        // default, or a bundle from a future version would launch the wrong CLI.
        match normalised.as_str() {
            "claude-code" | "claude" | "claudecode" => Ok(Agent::ClaudeCode),
            "codex" => Ok(Agent::Codex),
            other => Err(crate::error::Error::UnknownAgent {
                requested: other.to_string(),
                known: Agent::all()
                    .iter()
                    .map(|a| a.id())
                    .collect::<Vec<_>>()
                    .join(", "),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_through_from_str() {
        for agent in Agent::all() {
            assert_eq!(
                agent.id().parse::<Agent>().expect("id must parse"),
                *agent,
                "an agent's own id must parse back to it, or a stored project resolves wrong"
            );
        }
    }

    #[test]
    fn unknown_agents_are_refused_not_defaulted() {
        // The whole point: a value we do not recognise must fail, not quietly
        // become Claude Code. A bundle naming a future agent must refuse on an
        // old build rather than launch the wrong CLI against someone's session.
        let err = "gemini"
            .parse::<Agent>()
            .expect_err("unknown agent must fail");
        assert!(err.to_string().contains("gemini"));
        assert!(
            err.to_string().contains("claude-code") && err.to_string().contains("codex"),
            "the error must list the agents that ARE known: {err}"
        );
    }

    #[test]
    fn ids_are_stable_and_distinct() {
        let ids: Vec<&str> = Agent::all().iter().map(|a| a.id()).collect();
        assert_eq!(ids, vec!["claude-code", "codex"]);
        // Distinct commands, or attach would launch the wrong program.
        assert_ne!(Agent::ClaudeCode.command(), Agent::Codex.command());
    }
}
