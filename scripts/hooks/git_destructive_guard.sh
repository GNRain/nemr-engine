#!/usr/bin/env bash
# PreToolUse hook: refuse destructive git operations while uncommitted work
# exists — the structural form of the loop.md rule "commit or stash before any
# destructive git operation". Written rules failed twice (slips three and
# seven; the second while the rule was being consciously exercised elsewhere),
# which is what written-down-but-not-enforced looks like. This guard fires
# whether or not the session remembers the rule — the refuse_protected
# property, applied to git.
#
# DESIGN, earned the hard way across two adversarial review rounds (19 and 23
# confirmed breaks, every one a plainly-typed command):
#   - the command is PARSED, never regex-matched: a quote-aware pre-scan
#     strips bash comments and heredoc BODIES (data, not code), then POSIX
#     shlex tokenizes with newline/backtick as command separators and bash's
#     line-continuation rule;
#   - classification is by SUBCOMMAND TOKEN; long options match by git's own
#     unique-abbreviation rule; `--` ends flag scanning;
#   - where classification needs repo knowledge the guard asks git READ-ONLY
#     (rev-parse --verify for branch-vs-pathspec, ls-files for tracked paths,
#     worktree list for basenames) instead of guessing;
#   - each invocation resolves its own target tree: -C is cumulative like
#     git's, --work-tree in both forms, cd/pushd/popd tracked with a stack, a
#     cd that cannot succeed (or CDPATH ambiguity) makes the tree UNKNOWN;
#   - wrapper prefixes (sudo/env/timeout/time/nice/...) are stripped; if a
#     known wrapper's flags defeat the strip, the first `git` token after it
#     is taken as the invocation — inside wrapper context only.
#
# The guard READS state and refuses; it never repairs (no auto-stash — a
# control that repairs what it observes is the F-123 shape). It refuses
# exactly the rule's precondition, nothing stricter:
#   - a clean tree passes ("commit or stash FIRST" is what makes it clean);
#   - `git restore --staged` alone passes even dirty — it only unstages.
#     An over-claiming guard teaches reaching for the override, and then the
#     override stops carrying information;
#   - an UNKNOWN target tree, or an unreadable `git status`, REFUSES rather
#     than passing as clean — F-109: a failed read must not be
#     indistinguishable from a negative answer;
#   - `git stash drop|clear` refuses regardless of tree state: a stash holds
#     exactly the work the rule protects, one command after it was stashed.
#
# Deliberate destruction is declared per invocation — the override counts
# only as an environment prefix of the git invocation itself:
#     NEMR_GIT_DESTRUCTIVE_OK=1 git checkout -- src/config.rs
#
# The threat model is a session forgetting the rule, not one evading the
# guard; every slip on record was a plainly typed command. Known residue,
# accepted and documented: backticks inside double quotes, and a command
# assembled in shell variables, are not seen — both refuse-by-construction
# is impossible without running bash itself.
#
# Exit 0 = allow. Exit 2 = refuse (stderr shown to the session). Exit 1 =
# hook error on a malformed payload (non-blocking, by the hook contract).
set -euo pipefail

PROG=$(cat <<'PY'
import json, os, subprocess, sys

def refuse(lines):
    sys.stderr.write("\n".join(lines) + "\n")
    sys.exit(2)

RULE = "rule: commit or stash before any destructive git operation (.claude/loop.md)."
HOWTO = [
    "commit or stash first. If destroying uncommitted state IS the intent (e.g.",
    "restoring a deliberate mutation), declare it on that one invocation:",
    "    NEMR_GIT_DESTRUCTIVE_OK=1 <the command>",
]

try:
    payload = json.load(sys.stdin)
    cmd = payload.get("tool_input", {}).get("command", "")
    cwd = payload.get("cwd", ".")
except Exception:
    sys.exit(1)

if "git" not in cmd:
    sys.exit(0)


def sanitize(src):
    """Strip bash comments and heredoc BODIES, quote-aware. Comments end at
    newline (shlex's own comment handling swallows the newline separator);
    heredoc bodies are data to bash but code to shlex — an apostrophe in one
    tripped the unbalanced-quote hatch, and a `cd` line in one corrupted the
    directory tracking. Both review-round-two fail-opens."""
    out = []
    i, n = 0, len(src)
    state = "n"  # n normal, s single-quote, d double-quote
    heredocs = []  # delimiters queued on the current logical line
    at_word_start = True
    while i < n:
        c = src[i]
        if state == "s":
            out.append(c)
            if c == "'":
                state = "n"
            i += 1
            continue
        if state == "d":
            if c == "\\" and i + 1 < n:
                out.append(c)
                out.append(src[i + 1])
                i += 2
                continue
            out.append(c)
            if c == '"':
                state = "n"
            i += 1
            continue
        if c == "\\" and i + 1 < n:
            out.append(c)
            out.append(src[i + 1])
            at_word_start = False
            i += 2
            continue
        if c == "'":
            state = "s"
            out.append(c)
            at_word_start = False
            i += 1
            continue
        if c == '"':
            state = "d"
            out.append(c)
            at_word_start = False
            i += 1
            continue
        if c == "#" and at_word_start:
            while i < n and src[i] != "\n":
                i += 1
            continue
        if c.isdigit() and at_word_start:
            # a bash fd number is a digit IMMEDIATELY before <,> (2>/dev/null);
            # with a space it is a real argument. shlex loses adjacency, so
            # the distinction must be made here, where it still exists.
            j = i
            while j < n and src[j].isdigit():
                j += 1
            if j < n and src[j] in "<>":
                i = j  # drop the fd number; the operator is handled next
                continue
        if src.startswith("<<", i) and not src.startswith("<<<", i):
            j = i + 2
            if j < n and src[j] == "-":
                j += 1
            while j < n and src[j] in " \t":
                j += 1
            delim = ""
            if j < n and src[j] in "'\"":
                q = src[j]
                j += 1
                while j < n and src[j] != q:
                    delim += src[j]
                    j += 1
                j += 1
            else:
                while j < n and src[j] not in " \t\n;&|()<>":
                    delim += src[j]
                    j += 1
            heredocs.append(delim)
            out.append(" ")
            at_word_start = False
            i = j
            continue
        if c == "\n":
            out.append("\n")
            i += 1
            while heredocs:
                delim = heredocs.pop(0)
                while i < n:
                    k = src.find("\n", i)
                    line = src[i:] if k == -1 else src[i:k]
                    i = n if k == -1 else k + 1
                    if line == delim or line.lstrip("\t") == delim:
                        break
            at_word_start = True
            continue
        out.append(c)
        at_word_start = c in " \t;&|()"
        i += 1
    return "".join(out)


import shlex
try:
    # Bash deletes backslash-newline (line continuation); inside single
    # quotes this only mutates data bytes, never a flag or subcommand.
    text = sanitize(cmd.replace("\\\n", ""))
    # Newline and backtick are COMMAND SEPARATORS: the second line's (or the
    # substitution's) git invocation must not parse as arguments of the
    # first's. shlex's own '#' comments are disabled — sanitize() already
    # handled comments with bash's rules.
    lex = shlex.shlex(text, posix=True, punctuation_chars=";()|&<>\n`")
    lex.whitespace = " \t\r"
    lex.whitespace_split = True
    lex.commenters = ""
    tokens = list(lex)
except ValueError:
    # Unbalanced quoting AFTER heredoc bodies were removed: bash itself will
    # refuse to run this, so nothing can be destroyed by allowing it.
    sys.exit(0)


def is_sep(t):
    return bool(t) and all(c in ";&|()\n`" for c in t)


def is_redir(t):
    # A real redirection token from shlex is a pure punctuation run; a
    # DEQUOTED data token like "<html>" contains word characters and must
    # stay an ordinary word (round three: any() here swallowed the following
    # separator and hid the next command).
    return bool(t) and all(c in ";&|()<>" for c in t) and any(c in "<>" for c in t)


def resolve(base, p):
    """Resolve p against base; None = unresolvable (unknown tree)."""
    if base is None or p is None or "$" in p or "`" in p:
        return None
    p = os.path.expanduser(p)
    if not os.path.isabs(p):
        p = os.path.normpath(os.path.join(base, p))
    return p


# Split into simple commands at separators, tracking the working directory
# across cd/pushd/popd and ( ) subshells. A cd that cannot succeed makes the
# directory UNKNOWN — with `;` chaining bash stays put and runs the next
# command in the OLD directory, so trusting the failed target fails open.
simples = []
cur = cwd
parens = []
pushes = []
acc = []
skip_next = False
pending = []


def apply_dir_command(toks):
    global cur
    name = toks[0]
    arg = next((a for a in toks[1:] if not a.startswith("-") or a == "-"), None)
    if name == "popd":
        cur = pushes.pop() if pushes else None
        return
    if name == "pushd":
        pushes.append(cur)
    if arg is None:
        cur = os.path.expanduser("~") if name == "cd" else None
        return
    if arg == "-":
        cur = None
        return
    if (
        name == "cd"
        and not os.path.isabs(arg)
        and not arg.startswith(("./", "../"))
        and os.environ.get("CDPATH")
    ):
        cur = None  # CDPATH may send bash somewhere path arithmetic cannot see
        return
    dest = resolve(cur, arg)
    cur = dest if dest is not None and os.path.isdir(dest) else None


def flush_and_parens(sep_chars):
    """Record the accumulated simple command(s) BEFORE handling parens: a
    command inside (...) belongs to the subshell's directory, not the
    restored one. Then apply ( ) pushes/pops from the separator itself."""
    global acc, pending, cur
    if acc:
        pending.append(list(acc))
        acc = []
    for toks in pending:
        simples.append((toks, cur))
        if toks and toks[0] in ("cd", "pushd", "popd"):
            apply_dir_command(toks)
    pending = []
    for c in sep_chars:
        if c == "(":
            parens.append(cur)
        elif c == ")":
            cur = parens.pop() if parens else cur


for t in tokens:
    if skip_next:
        skip_next = False
        continue
    if is_sep(t):
        flush_and_parens(t)
        continue
    if is_redir(t):
        if "(" in t or ")" in t:
            # process substitution <(...) / >(...): its content is a real
            # command bash runs — a command boundary, and its first word
            # must not be swallowed as a redirection filename.
            flush_and_parens(t)
            continue
        # plain redirection: skip the filename that follows (fd numbers were
        # already stripped by sanitize, where adjacency still existed).
        skip_next = True
        continue
    acc.append(t)
flush_and_parens(";")  # unconditional: a trailing redirection must not eat the flush

ASSIGN_OK = "NEMR_GIT_DESTRUCTIVE_OK=1"
WRAPPERS = {"command", "exec", "env", "nohup", "sudo", "time", "nice",
            "stdbuf", "ionice", "timeout"}
# Shell reserved words at command position hide the command they prefix
# ("if git reset --hard; then" — branching on a command's exit is idiomatic).
# Stripped like wrappers. Consequence, accepted and documented: a function
# DEFINITION whose body opens with destructive git refuses too — the call
# site ("f") is invisible to any static guard, so the definition is the only
# enforceable point; the override covers a deliberate one.
RESERVED = {"if", "then", "elif", "else", "fi", "while", "until", "do",
            "done", "for", "case", "esac", "{", "}", "!", "[[", "]]"}
VALUE_FLAGS = {
    "sudo": {"-u", "-g", "-h", "-p", "-C", "-D", "-R", "-T", "-U"},
    "env": {"-u", "-C"},
    "timeout": {"-k", "--kill-after", "-s", "--signal"},
    "nice": {"-n"},
    "ionice": {"-c", "-n", "-p"},
    "stdbuf": set(),
}


def is_assignment(t):
    if "=" not in t or t.startswith("-"):
        return False
    name = t.split("=", 1)[0]
    return bool(name) and (name[0].isalpha() or name[0] == "_") and all(
        c.isalnum() or c == "_" for c in name
    )


targets = []  # (target_dir_or_None, why)
always = []   # refused regardless of tree state


def git_ok(base, *args):
    """Read-only question to git; False on any failure."""
    if base is None:
        return False
    r = subprocess.run(
        ["git", "-C", base, *args], capture_output=True, text=True
    )
    return r.returncode == 0


for toks, base in simples:
    override = False
    saw_wrapper = False
    i = 0
    while i < len(toks):
        t = toks[i]
        if is_assignment(t):
            if t == ASSIGN_OK:
                override = True
            i += 1
            continue
        if t in RESERVED:
            i += 1
            continue
        if t in WRAPPERS:
            saw_wrapper = True
            w = t
            i += 1
            while i < len(toks) and toks[i].startswith("-"):
                flag = toks[i]
                i += 1
                if w == "env" and flag == "-S" and i < len(toks):
                    # env -S SPLITS its value and runs it as the command:
                    # re-lex the string in place so its git is a git.
                    try:
                        toks[i:i] = shlex.split(toks.pop(i))
                    except ValueError:
                        if "git" in toks[i]:
                            targets.append((None, "env -S with an unparseable command string"))
                        i += 1
                    continue
                if flag in VALUE_FLAGS.get(w, set()) and i < len(toks):
                    i += 1
            if w == "timeout" and i < len(toks):
                i += 1  # the duration argument
            continue
        break
    toks = toks[i:]
    if not toks:
        continue
    name = toks[0]
    if name != "git" and not name.endswith("/git"):
        if saw_wrapper and "git" in toks:
            # A wrapper flag the strip did not model consumed tokens up to
            # here; inside wrapper context the first `git` token is the
            # command (review round two: env -i / sudo -u forms failed open).
            toks = toks[toks.index("git"):]
        else:
            continue

    # git global options (cumulative -C, both --work-tree forms), then
    # subcommand, then args.
    tgt = base
    sub = None
    args = []
    j = 1
    while j < len(toks):
        t = toks[j]
        if sub is None and t == "-C" and j + 1 < len(toks):
            tgt = resolve(tgt, toks[j + 1])
            j += 2
            continue
        if sub is None and t in ("-c",) and j + 1 < len(toks):
            j += 2
            continue
        if sub is None and t.startswith("--work-tree="):
            tgt = resolve(tgt, t.split("=", 1)[1])
            j += 1
            continue
        if sub is None and t == "--work-tree" and j + 1 < len(toks):
            tgt = resolve(tgt, toks[j + 1])
            j += 2
            continue
        if sub is None and t == "--git-dir" and j + 1 < len(toks):
            j += 2
            continue
        if sub is None and t.startswith("-"):
            j += 1
            continue
        if sub is None:
            sub = t
            j += 1
            continue
        args.append(t)
        j += 1
    if sub is None:
        continue

    pre = args[: args.index("--")] if "--" in args else args
    ddash = "--" in args

    def flags(short, exclude=()):
        for a in pre:
            if a.startswith("--") or not a.startswith("-"):
                continue
            if any(a.startswith(x) for x in exclude):
                continue
            if short in a[1:]:
                return True
        return False

    def has_long(*longs):
        # git accepts unique abbreviations of long options; --h matching
        # both --hard and --help means git errors, so over-matching there
        # only refuses a command git itself rejects.
        for a in pre:
            if not a.startswith("--") or len(a) < 3:
                continue
            stem = a.split("=", 1)[0]
            for want in longs:
                if want.startswith(stem):
                    return True
        return False

    def positional(skip_value_of=()):
        out = []
        skip = False
        for a in args:
            if a == "--":
                break
            if skip:
                skip = False
                continue
            if a in skip_value_of:
                skip = True
                continue
            if a.startswith("-"):
                continue
            out.append(a)
        return out

    why = None
    always_why = None
    if sub == "reset" and has_long("--hard", "--merge"):
        why = "git reset --hard/--merge"
    elif sub == "checkout":
        if (
            ddash
            or has_long("--force", "--pathspec-from-file")
            or flags("f", exclude=("-b", "-B"))
        ):
            why = "git checkout with -- / --force / --pathspec-from-file"
        else:
            for a in positional(skip_value_of=("-b", "-B", "--orphan")):
                if tgt is None:
                    why = "git checkout <arg> against an unknown tree"
                    break
                if git_ok(tgt, "rev-parse", "--verify", "--quiet", a + "^{commit}"):
                    continue  # a committish: branch/tag switch, not a pathspec
                if git_ok(tgt, "ls-files", "--error-unmatch", "--", a):
                    why = "git checkout <tracked path> (overwrites the worktree copy)"
                    break
    elif sub == "switch" and (
        has_long("--force", "--discard-changes") or flags("f", exclude=("-c", "-C"))
    ):
        why = "git switch --force/--discard-changes"
    elif sub == "restore":
        staged = has_long("--staged") or flags("S", exclude=("-s",))
        worktree = has_long("--worktree") or flags("W")
        if worktree or not staged:
            why = "git restore reaching the worktree"
    elif sub == "clean":
        force = has_long("--force") or flags("f")
        dry = has_long("--dry-run") or flags("n")
        if force and not dry:
            why = "git clean --force"
    elif sub == "stash":
        action = next((a for a in args if not a.startswith("-")), "")
        if action in ("drop", "clear"):
            always_why = (
                "git stash %s destroys a stash — which holds exactly the work "
                "the rule protects, one command after it was stashed" % action
            )
    elif sub == "merge" and has_long("--abort"):
        why = "git merge --abort (discards in-progress resolution work)"
    elif sub == "rebase" and not has_long("--continue"):
        why = "git rebase" + (" --abort" if has_long("--abort") else "")
    elif sub == "rm":
        if (has_long("--force") or flags("f")) and not has_long("--cached"):
            why = "git rm --force"
    elif sub == "worktree" and args and args[0] == "remove" and (
        has_long("--force") or flags("f")
    ):
        wt = next((a for a in args[1:] if not a.startswith("-")), None)
        if wt is not None:
            why = "git worktree remove --force"
            wt_path = resolve(tgt, wt)
            if wt_path is None or not os.path.isdir(wt_path):
                # git also accepts a unique basename: ask the repo.
                wt_path = None
                if tgt is not None:
                    r = subprocess.run(
                        ["git", "-C", tgt, "worktree", "list", "--porcelain"],
                        capture_output=True, text=True,
                    )
                    if r.returncode == 0:
                        for line in r.stdout.splitlines():
                            if line.startswith("worktree ") and (
                                line.split(" ", 1)[1].rstrip("/").endswith("/" + wt)
                            ):
                                wt_path = line.split(" ", 1)[1]
                                break
                        else:
                            why = None  # no such worktree: git errors on its own
                    # listing failed: wt_path stays None -> fail closed
            tgt = wt_path if why is not None else tgt

    if always_why is not None:
        if not override:
            always.append(always_why)
        continue
    if why is None or override:
        continue
    targets.append((tgt, why))

if always:
    refuse(["git guard: REFUSED — " + always[0] + "."] + [RULE] + HOWTO)

for tgt, why in targets:
    if tgt is None:
        refuse([
            "git guard: REFUSED — cannot determine which tree this targets "
            "(an unresolvable cd/-C, or one that would fail): " + why + ".",
            "a target that cannot be verified must not pass as clean (F-109).",
        ] + HOWTO)
    if not os.path.isdir(tgt):
        continue  # git itself will fail before destroying anything
    rp = subprocess.run(
        ["git", "-C", tgt, "rev-parse", "--is-inside-work-tree"],
        capture_output=True, text=True,
    )
    if rp.returncode != 0:
        if "not a git repository" in rp.stderr:
            continue  # a genuine non-repo: git refuses on its own
        refuse([
            "git guard: REFUSED — could not read the repository state in " + tgt + ":",
            "    " + rp.stderr.strip(),
            "a failed read must not pass as a clean answer (F-109).",
        ])
    st = subprocess.run(
        ["git", "-C", tgt, "status", "--porcelain"],
        capture_output=True, text=True,
    )
    if st.returncode != 0:
        refuse([
            "git guard: REFUSED — could not read 'git status' to verify the tree is clean:",
            "    " + st.stderr.strip(),
            "a failed read must not pass as a clean answer (F-109).",
        ])
    if st.stdout.strip():
        refuse(
            ["git guard: REFUSED — destructive git operation (" + why + ") with "
             "uncommitted work present.", RULE, "uncommitted in " + tgt + ":"]
            + ["    " + l for l in st.stdout.rstrip("\n").split("\n")]
            + HOWTO
        )

sys.exit(0)
PY
)
exec python3 -c "$PROG"
