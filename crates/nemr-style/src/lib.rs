//! One voice for every nemr command.
//!
//! Four rules, from the Product Owner (2026-09-11), and everything here exists
//! to make them cheap to follow:
//!
//! 1. **Result first.** The first line says whether it worked.
//! 2. **Colour with meaning, never decoration.** Green is success, or a state
//!    that is fine. Amber is pending, in progress, or a warning. Red is
//!    failure, or a state needing action. Dim is secondary detail.
//! 3. **Failures name the cause and the fix**, and the fix is a command.
//! 4. **Values the user acts on get colour; labels do not.** In a status
//!    block, `no` is the word that matters, not `running:`.
//!
//! Colour is decided once, by the same rule the installer uses: a terminal,
//! no `NO_COLOR`, and a `TERM` that is not `dumb`. With colour off the marks
//! become `OK` / `XX` / `!!`, because a tick that renders as a box is worse
//! than two letters that do not.

use std::io::IsTerminal;

/// What a piece of text MEANS. Never what colour it is — the mapping lives in
/// one place so that "amber for pending" is a decision made once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// It worked, or this state is fine.
    Good,
    /// In progress, or a warning: nothing is broken, but do look.
    Pending,
    /// It failed, or this state needs action.
    Bad,
    /// Secondary detail: labels, paths, things you read only if you care.
    Dim,
    /// No opinion.
    Plain,
}

impl Tone {
    fn sgr(self) -> &'static str {
        match self {
            Tone::Good => "\x1b[32m",
            Tone::Pending => "\x1b[33m",
            Tone::Bad => "\x1b[31m",
            Tone::Dim => "\x1b[2m",
            Tone::Plain => "",
        }
    }
}

/// The decision about this terminal, made once and carried.
#[derive(Clone, Copy, Debug)]
pub struct Voice {
    colour: bool,
}

impl Voice {
    /// For anything printed to stdout.
    pub fn for_stdout() -> Voice {
        Voice::decide(std::io::stdout().is_terminal())
    }

    /// For anything printed to stderr — refusals, usually.
    pub fn for_stderr() -> Voice {
        Voice::decide(std::io::stderr().is_terminal())
    }

    /// No colour, whatever the terminal says. For tests and for output that is
    /// going to be parsed.
    pub fn plain() -> Voice {
        Voice { colour: false }
    }

    /// Colour, whatever the terminal says. For tests that need to see it.
    pub fn coloured() -> Voice {
        Voice { colour: true }
    }

    fn decide(is_tty: bool) -> Voice {
        let no_colour = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
        let dumb = std::env::var("TERM").map(|t| t == "dumb").unwrap_or(false);
        Voice {
            colour: is_tty && !no_colour && !dumb,
        }
    }

    pub fn has_colour(&self) -> bool {
        self.colour
    }

    /// Text in a tone. With colour off this is the text, unchanged — never a
    /// substitute marker, because a reader without colour is reading words.
    pub fn paint(&self, tone: Tone, text: &str) -> String {
        if !self.colour || tone == Tone::Plain {
            return text.to_string();
        }
        format!("{}{}\x1b[0m", tone.sgr(), text)
    }

    /// The mark that opens a result line. `OK`/`XX`/`!!` without colour, for
    /// the same reason the installer uses them: a tick that renders as a box
    /// is worse than two letters that do not.
    pub fn mark(&self, tone: Tone) -> &'static str {
        match (self.colour, tone) {
            (true, Tone::Good) => "✓",
            (true, Tone::Bad) => "✗",
            (true, Tone::Pending) => "⚠",
            (false, Tone::Good) => "OK",
            (false, Tone::Bad) => "XX",
            (false, Tone::Pending) => "!!",
            _ => " ",
        }
    }

    /// THE FIRST LINE. Everything a command prints starts with one of these,
    /// and a reader who stops after it knows what happened.
    pub fn result(&self, tone: Tone, headline: &str) -> String {
        // The MARK carries the colour; the headline is plain, because a whole
        // sentence in red is harder to read than a red mark in front of one.
        format!("{} {}", self.paint(tone, self.mark(tone)), headline)
    }

    pub fn done(&self, headline: &str) -> String {
        self.result(Tone::Good, headline)
    }

    pub fn failed(&self, headline: &str) -> String {
        self.result(Tone::Bad, headline)
    }

    pub fn warned(&self, headline: &str) -> String {
        self.result(Tone::Pending, headline)
    }

    /// A label and a value, aligned. The LABEL is dim and the VALUE carries
    /// the tone, because the value is the thing being reported.
    pub fn field(&self, label: &str, value: &str, tone: Tone) -> String {
        let pad = LABEL_WIDTH.saturating_sub(label.chars().count() + 1);
        format!(
            "  {}{}{} {}",
            self.paint(Tone::Dim, label),
            self.paint(Tone::Dim, ":"),
            " ".repeat(pad),
            self.paint(tone, value)
        )
    }

    /// Pad a cell that may carry colour, to a COLUMN width — the escape bytes
    /// are invisible and `{:<9}` would count them, walking the column right by
    /// however many bytes the colour took. Used by the table listings, whose
    /// exact columns three acceptance scripts match on.
    pub fn pad(&self, painted: &str, visible: usize, width: usize) -> String {
        let mut out = String::from(painted);
        for _ in visible..width {
            out.push(' ');
        }
        out
    }

    /// A continuation under a field: the same indent, no label, no colon —
    /// for the sentence that explains the value above it.
    pub fn note(&self, text: &str) -> String {
        // LABEL_WIDTH + 1: `field` spends one more space between the padding
        // and the value, and a continuation that does not line up with the
        // value it continues is just a stray sentence.
        format!(
            "  {}{}",
            " ".repeat(LABEL_WIDTH + 1),
            self.paint(Tone::Dim, text)
        )
    }

    /// A whole refusal: what failed, why, and the command that fixes it. The
    /// shape the installer uses, so a refusal reads the same wherever it comes
    /// from.
    pub fn refusal(&self, headline: &str, because: &str, fix: &str) -> String {
        let mut out = String::new();
        out.push('\n');
        out.push_str(&self.failed(headline));
        out.push_str("\n\n");
        out.push_str(&self.field("Because", because, Tone::Plain));
        out.push('\n');
        out.push_str(&self.field("Fix", fix, Tone::Plain));
        out.push('\n');
        out
    }
}

/// Render an error the way the installer renders a failure: the first line
/// says what went wrong, and a line that looks like a remedy becomes the Fix.
///
/// Most of this project's errors already end with the command that fixes them
/// — `Create it first: nemr create x --size 2GB` — because somebody wrote them
/// that way one at a time. This gives every one of them the same shape without
/// rewriting them: the first line is the headline, a later line carrying a
/// `word: command` remedy becomes `Fix`, and anything else is dim detail
/// underneath.
pub fn error_block(v: &Voice, message: &str, causes: &[String]) -> String {
    let mut lines = message.lines().map(str::trim_end).filter(|l| !l.is_empty());
    let headline = lines.next().unwrap_or("it failed").to_string();
    let rest: Vec<&str> = lines.collect();
    let mut out = String::new();
    out.push('\n');
    out.push_str(&v.failed(headline.trim_end_matches('.')));
    out.push_str(".\n");
    let mut said_fix = false;
    for line in &rest {
        // A remedy is a line whose second half is a command — `Fix: nemr x`,
        // `Create it first: nemr create …`, `Run: nemr login`.
        match line.split_once(": ") {
            Some((lead, cmd)) if cmd.starts_with("nemr") || cmd.starts_with("./") => {
                if !said_fix {
                    out.push('\n');
                    said_fix = true;
                }
                let _ = lead;
                out.push_str(&v.field("Fix", cmd.trim(), Tone::Good));
                out.push('\n');
            }
            _ => {
                out.push_str(&v.note(line.trim()));
                out.push('\n');
            }
        }
    }
    for c in causes {
        out.push_str(&v.note(c));
        out.push('\n');
    }
    out
}

/// The column the values line up in. Wide enough for the longest label any
/// command uses, so two commands' output can be read as one.
const LABEL_WIDTH: usize = 11;

/// A line that is rewritten in place while something long runs, and erased
/// when it ends — the installer's behaviour, in Rust.
///
/// It draws only where the installer would: on a terminal, with colour
/// allowed. Anywhere else it is silent rather than emitting a hundred lines
/// into a log, and the command's ordinary output is the whole story.
pub struct Progress {
    voice: Voice,
    drawn: usize,
}

impl Progress {
    pub fn new(voice: Voice) -> Progress {
        Progress { voice, drawn: 0 }
    }

    /// Show `text` where the last one was.
    pub fn set(&mut self, text: &str) {
        if !self.voice.colour {
            return;
        }
        use std::io::Write;
        let line = format!("  {}", self.voice.paint(Tone::Pending, text));
        let mut out = std::io::stdout();
        let _ = write!(out, "\r\x1b[K{line}");
        let _ = out.flush();
        self.drawn = line.len();
    }

    /// Erase the line. The caller prints the result itself, because the result
    /// is not progress and should survive being piped into a file.
    pub fn clear(&mut self) {
        if !self.voice.colour || self.drawn == 0 {
            return;
        }
        use std::io::Write;
        let mut out = std::io::stdout();
        let _ = write!(out, "\r\x1b[K");
        let _ = out.flush();
        self.drawn = 0;
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        self.clear();
    }
}

/// Bytes, for a progress line: the number a person can hold in their head.
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u + 1 < UNITS.len() {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else if v >= 100.0 {
        format!("{v:.0} {}", UNITS[u])
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_colour_nothing_carries_an_escape() {
        let v = Voice::plain();
        for s in [
            v.done("it worked"),
            v.failed("it did not"),
            v.warned("look at this"),
            v.field("running", "no", Tone::Bad),
            v.refusal("no", "because", "run this"),
        ] {
            assert!(!s.contains('\x1b'), "plain output must be plain: {s:?}");
        }
    }

    #[test]
    fn the_marks_are_words_when_there_is_no_colour() {
        let plain = Voice::plain();
        assert!(plain.done("x").starts_with("OK "));
        assert!(plain.failed("x").starts_with("XX "));
        assert!(plain.warned("x").starts_with("!! "));
        let col = Voice::coloured();
        assert!(col.done("x").contains('✓'));
        assert!(col.failed("x").contains('✗'));
        assert!(col.warned("x").contains('⚠'));
    }

    #[test]
    fn the_value_carries_the_colour_and_the_label_does_not() {
        let v = Voice::coloured();
        let line = v.field("running", "no", Tone::Bad);
        let (label, value) = line.split_once(':').unwrap();
        assert!(
            label.contains("\x1b[2m"),
            "the label is dim, not coloured: {label:?}"
        );
        assert!(
            !label.contains("\x1b[31m"),
            "the label is not red: {label:?}"
        );
        assert!(value.contains("\x1b[31m"), "the value is red: {value:?}");
    }

    #[test]
    fn values_line_up_across_commands() {
        let v = Voice::plain();
        let a = v.field("running", "yes", Tone::Good);
        let b = v.field("address", "127.0.0.1:8080", Tone::Plain);
        let col = |s: &str| s.find(|c: char| !c.is_whitespace() && c != ':').unwrap();
        // The value starts in the same column in both, which is what makes two
        // commands' output read as one.
        assert_eq!(
            a.rfind("yes").unwrap(),
            b.rfind("127.0.0.1:8080").unwrap(),
            "{a:?} vs {b:?}"
        );
        let _ = col;
    }

    #[test]
    fn a_refusal_says_why_and_what_to_run() {
        let v = Voice::plain();
        let r = v.refusal(
            "it did not work",
            "the database did not answer",
            "./scripts/x.sh",
        );
        assert!(r.contains("XX it did not work"));
        assert!(r.contains("Because"));
        assert!(r.contains("the database did not answer"));
        assert!(r.contains("Fix"));
        assert!(r.contains("./scripts/x.sh"));
    }

    #[test]
    fn a_coloured_cell_is_padded_by_what_you_can_see() {
        let v = Voice::coloured();
        let painted = v.paint(Tone::Good, "running");
        let cell = v.pad(&painted, "running".len(), 9);
        // Two spaces after the word, however many bytes the colour took.
        assert!(cell.ends_with("  "), "{cell:?}");
        assert_eq!(
            cell.chars().filter(|c| *c == ' ').count(),
            2,
            "exactly the padding, and no more: {cell:?}"
        );
        // And with no colour it is the plain `{:<9}` it replaces.
        let plain = Voice::plain();
        assert_eq!(plain.pad("running", 7, 9), format!("{:<9}", "running"));
    }

    #[test]
    fn a_note_has_no_label_and_no_colon() {
        let v = Voice::plain();
        let n = v.note("started by this command");
        assert!(!n.contains(':'), "{n:?}");
        // And it lines up under the values, not under the labels.
        let f = v.field("running", "yes", Tone::Good);
        assert_eq!(
            n.find("started").unwrap(),
            f.find("yes").unwrap(),
            "{n:?} vs {f:?}"
        );
    }

    #[test]
    fn an_error_keeps_its_headline_and_promotes_its_remedy() {
        let v = Voice::plain();
        let b = error_block(
            &v,
            "no project named \"x\".\nCreate it first: nemr create x --size 2GB",
            &[],
        );
        assert!(b.contains("XX no project named \"x\"."), "{b}");
        assert!(b.contains("Fix"), "{b}");
        assert!(b.contains("nemr create x --size 2GB"), "{b}");
        // A cause is detail, not a headline.
        let c = error_block(&v, "it broke", &["No such file or directory".into()]);
        assert!(c.starts_with("\nXX it broke."), "{c:?}");
        assert!(c.contains("No such file or directory"), "{c}");
    }

    #[test]
    fn bytes_are_readable() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(1024), "1.0 KiB");
        assert_eq!(bytes(107 * 1024 * 1024), "107 MiB");
        assert_eq!(bytes(3 * 1024 * 1024 * 1024), "3.0 GiB");
    }
}
