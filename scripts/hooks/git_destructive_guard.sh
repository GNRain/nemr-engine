#!/usr/bin/env bash
# PreToolUse hook: refuse destructive git operations while uncommitted work
# exists — the structural form of the loop.md rule "commit or stash before any
# destructive git operation". Written rules failed twice (slips three and
# seven; the second while the rule was being consciously exercised elsewhere),
# which is what written-down-but-not-enforced looks like. This guard fires
# whether or not the session remembers the rule — the refuse_protected
# property, applied to git.
#
# The command is PARSED (POSIX shlex: quotes, line continuations, operators),
# never regex-matched as text. A first draft used regex segmentation and an
# adversarial review broke it 19 ways with plainly-typed commands — `a & b`
# joining, backslash-newline continuations, quoted operators, and substring
# false-positives that refused `git commit -m "never run git reset --hard"`,
# the guard blocking its own remedy. Classification is by subcommand token;
# each git invocation resolves its own target tree (-C, --work-tree, and cd
# tracked across the command with a directory stack).
#
# The guard READS state and refuses; it never repairs (no auto-stash — a
# control that repairs what it observes is the F-123 shape). It refuses
# exactly the rule's precondition, nothing stricter:
#   - a clean tree passes ("commit or stash FIRST" is what makes it clean);
#   - `git restore --staged` alone passes even dirty — it only unstages.
#     An over-claiming guard teaches reaching for the override, and then the
#     override stops carrying information;
#   - a target that CANNOT be resolved (a $VAR path, an unreadable status)
#     REFUSES rather than passing as clean — F-109: a failed read must not be
#     indistinguishable from a negative answer;
#   - `git stash drop|clear` refuses regardless of tree state: a stash holds
#     exactly the work the rule protects, one command after it was stashed.
#
# Deliberate destruction is declared per invocation — the override counts
# only as an environment prefix of the git invocation itself, visible in the
# transcript as a recorded decision:
#     NEMR_GIT_DESTRUCTIVE_OK=1 git checkout -- src/config.rs
#
# The threat model is a session forgetting the rule, not one evading the
# guard; every slip on record was a plainly typed command.
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

import shlex
try:
    # Bash deletes backslash-newline (line continuation); shlex instead glues
    # the escaped newline onto the next word, hiding the subcommand token.
    # (Inside single quotes this only mutates data bytes, never a flag or
    # subcommand, so classification is unaffected.)
    cmd_parse = cmd.replace("\\\n", "")
    # Newline is a COMMAND SEPARATOR, not whitespace: otherwise the second
    # line's git invocation parses as arguments of the first line's.
    lex = shlex.shlex(cmd_parse, posix=True, punctuation_chars=";()|&<>\n")
    lex.whitespace = " \t\r"
    lex.whitespace_split = True
    tokens = list(lex)
except ValueError:
    # Unbalanced quoting: the shell itself will refuse to run this, so
    # nothing can be destroyed by allowing it through.
    sys.exit(0)

def is_sep(t):
    return bool(t) and all(c in ";&|()\n" for c in t)

def is_redir(t):
    return any(c in "<>" for c in t)

def resolve(base, p):
    """Resolve p against base; None = unresolvable (unknown tree)."""
    if base is None or p is None or "$" in p or "`" in p:
        return None
    p = os.path.expanduser(p)
    if not os.path.isabs(p):
        p = os.path.normpath(os.path.join(base, p))
    return p

# Split the token stream into simple commands at list/pipe separators,
# tracking the working directory across cd and ( ) subshells.
simples = []  # (tokens, curdir_at_that_point, override_declared)
cur = cwd
dirstack = []
acc = []
skip_next = False
pending = []
for t in tokens + [";"]:
    if skip_next:
        skip_next = False
        continue
    if is_sep(t):
        if acc:
            pending.append(list(acc))
            acc = []
        # Flush BEFORE handling parens: a command inside (...) is recorded
        # against the subshell's directory, not the restored one.
        for toks in pending:
            simples.append((toks, cur))
            if toks and toks[0] == "cd":
                arg = next((a for a in toks[1:] if not a.startswith("-") or a == "-"), None)
                if arg is None:
                    cur = os.path.expanduser("~")
                elif arg == "-":
                    cur = None
                else:
                    cur = resolve(cur, arg)
        pending = []
        for c in t:
            if c == "(":
                dirstack.append(cur)
            elif c == ")":
                cur = dirstack.pop() if dirstack else cur
        continue
    if is_redir(t):
        skip_next = True
        continue
    acc.append(t)

ASSIGN_OK = "NEMR_GIT_DESTRUCTIVE_OK=1"
WRAPPERS = {"command", "exec", "env", "nohup", "sudo"}

targets = []   # (target_dir_or_None, why)
always = []    # (why,) — refused regardless of tree state

for toks, base in simples:
    override = False
    i = 0
    while i < len(toks):
        t = toks[i]
        if "=" in t and t.split("=", 1)[0].replace("_", "").isalnum() and not t.startswith("-"):
            if t == ASSIGN_OK:
                override = True
            i += 1
            continue
        if t in WRAPPERS:
            i += 1
            continue
        if t == "timeout" and i + 1 < len(toks):
            i += 2
            continue
        break
    toks = toks[i:]
    if not toks:
        continue
    name = toks[0]
    if name != "git" and not name.endswith("/git"):
        continue

    # git global options, then subcommand, then args.
    gitdir_opt = None
    sub = None
    args = []
    j = 1
    while j < len(toks):
        t = toks[j]
        if sub is None and t == "-C" and j + 1 < len(toks):
            gitdir_opt = toks[j + 1]
            j += 2
            continue
        if sub is None and t == "-c" and j + 1 < len(toks):
            j += 2
            continue
        if sub is None and t.startswith("--work-tree="):
            gitdir_opt = t.split("=", 1)[1]
            j += 1
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

    tgt = resolve(base, gitdir_opt) if gitdir_opt is not None else base

    def flags(short):
        """True if any pre-`--` single-dash token carries `short`, or the
        long form is present."""
        for a in args:
            if a == "--":
                break
            if a.startswith("--"):
                continue
            if a.startswith("-") and short in a[1:]:
                return True
        return False

    def has(*longs):
        for a in args:
            if a == "--":
                break
            if a in longs:
                return True
        return False

    why = None
    always_why = None
    if sub == "reset" and (has("--hard", "--merge")):
        why = "git reset " + ("--hard" if has("--hard") else "--merge")
    elif sub == "checkout":
        if has("--") or has("-f", "--force") or flags("f"):
            why = "git checkout with -- / --force"
        else:
            for a in args:
                if a.startswith("-"):
                    continue
                prev = args[args.index(a) - 1] if args.index(a) > 0 else ""
                if prev in ("-b", "-B", "--orphan"):
                    continue
                if tgt is not None and os.path.exists(os.path.join(tgt, a)):
                    why = "git checkout <existing path> (overwrites the worktree copy)"
                    break
    elif sub == "switch" and (has("-f", "--force", "--discard-changes")):
        why = "git switch --force/--discard-changes"
    elif sub == "restore":
        staged = has("--staged") or flags("S")
        worktree = has("--worktree") or flags("W")
        if worktree or not staged:
            why = "git restore reaching the worktree"
    elif sub == "clean":
        force = has("--force") or flags("f")
        dry = has("--dry-run", "-n") or flags("n")
        if force and not dry:
            why = "git clean --force"
    elif sub == "stash" and args and args[0] in ("drop", "clear"):
        always_why = (
            "git stash %s destroys a stash — which holds exactly the work the "
            "rule protects, one command after it was stashed" % args[0]
        )
    elif sub == "merge" and has("--abort"):
        why = "git merge --abort (discards in-progress uncommitted resolution work)"
    elif sub == "rebase" and not has("--continue"):
        # --continue is the conflict-resolution flow itself (tree necessarily
        # dirty) and destroys nothing; every other rebase form can.
        why = "git rebase" + (" --abort" if has("--abort") else "")
    elif sub == "rm":
        force = has("--force") or flags("f")
        if force and not has("--cached"):
            why = "git rm --force"
    elif sub == "worktree" and args and args[0] == "remove" and (has("--force") or flags("f")):
        wt = next((a for a in args[1:] if not a.startswith("-")), None)
        tgt = resolve(base, wt) if wt is not None else None
        why = "git worktree remove --force"

    if always_why is not None:
        if not override:
            always.append(always_why)
        continue
    if why is None or override:
        continue
    targets.append((tgt, why))

if always:
    refuse(
        ["git guard: REFUSED — " + always[0] + "."]
        + [RULE]
        + HOWTO
    )

for tgt, why in targets:
    if tgt is None:
        refuse([
            "git guard: REFUSED — cannot determine which tree this targets "
            "(a cd or -C with an unexpanded variable): " + why + ".",
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
