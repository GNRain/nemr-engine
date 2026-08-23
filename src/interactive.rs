//! Resolving a value that may come from a flag, an interactive prompt, or a
//! default — without ever hanging.
//!
//! The hard requirement (WP-H): a command must NEVER block waiting for input
//! nobody can give. A `nemr create` that waits forever on a CI runner burns the
//! whole job timeout and reports nothing, which is the worst failure shape we
//! have.
//!
//! The design keeps the dangerous part — the decision of whether to prompt —
//! as a pure function with no I/O, so it is exhaustively testable with no TTY.
//! The actual prompting happens only in the one branch that pure function
//! blesses.

/// Whether the process may prompt: stdin is a terminal AND the user has not
/// forced non-interactive mode.
///
/// The env override exists for scripts, and for the day something has a terminal
/// but must not be asked questions (the E-09 daemon creating a project on a GUI
/// user's behalf).
pub fn is_interactive() -> bool {
    use std::io::IsTerminal;
    if std::env::var_os("NEMR_NON_INTERACTIVE").is_some() {
        return false;
    }
    std::io::stdin().is_terminal()
}

/// What to do about one field, decided without doing any I/O.
#[derive(Debug, PartialEq, Eq)]
pub enum Resolution<T> {
    /// A flag supplied it; use this and do not prompt.
    Provided(T),
    /// No flag, but we may prompt; go ask (pre-selecting `default` if any).
    Prompt { default: Option<T> },
    /// No flag and no terminal, but a default exists; use it silently.
    UseDefault(T),
    /// No flag, no terminal, no default: fail immediately, naming the flag.
    MustFail { required_flag: String },
}

/// Decide how to resolve one field. Pure — no I/O, no TTY, no prompting.
///
/// `provided` is the flag value (if the user passed one). `interactive` is
/// [`is_interactive`]'s answer, injected so this is testable. `default` is the
/// sensible fallback, if the field has one. `required_flag` names the flag to
/// cite when there is no way to proceed.
pub fn decide<T>(
    provided: Option<T>,
    interactive: bool,
    default: Option<T>,
    required_flag: &str,
) -> Resolution<T> {
    // A flag always wins, in every mode. This is what keeps the flag path — the
    // one CI, the tests and the daemon use — the real path, with the prompt
    // merely filling in.
    if let Some(value) = provided {
        return Resolution::Provided(value);
    }
    if interactive {
        return Resolution::Prompt { default };
    }
    // Non-interactive from here: never prompt.
    match default {
        Some(value) => Resolution::UseDefault(value),
        None => Resolution::MustFail {
            required_flag: required_flag.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The four cases, exhaustively. These are the whole safety argument.

    #[test]
    fn a_flag_always_wins_even_interactively() {
        assert_eq!(
            decide(Some(7), true, Some(0), "--n"),
            Resolution::Provided(7),
            "a provided flag must never be second-guessed by a prompt"
        );
        assert_eq!(
            decide(Some(7), false, Some(0), "--n"),
            Resolution::Provided(7)
        );
    }

    #[test]
    fn interactive_and_missing_prompts_with_the_default_preselected() {
        assert_eq!(
            decide(None, true, Some(2), "--n"),
            Resolution::Prompt { default: Some(2) }
        );
        assert_eq!(
            decide::<i32>(None, true, None, "--n"),
            Resolution::Prompt { default: None }
        );
    }

    #[test]
    fn non_interactive_missing_with_a_default_uses_it_silently() {
        assert_eq!(
            decide(None, false, Some(2), "--n"),
            Resolution::UseDefault(2),
            "no terminal but a default exists: proceed, do NOT hang"
        );
    }

    #[test]
    fn non_interactive_missing_with_no_default_fails_immediately() {
        // THE case this whole module exists for: no flag, no terminal, no
        // default. It must fail fast naming the flag — never wait for input.
        assert_eq!(
            decide::<i32>(None, false, None, "--agent"),
            Resolution::MustFail {
                required_flag: "--agent".to_string()
            }
        );
    }
}
