#!/usr/bin/env bash
# Acceptance suite for scripts/hooks/git_destructive_guard.sh — every vector
# drives the REAL hook script with the REAL PreToolUse payload shape. The
# refused set includes the reproducing input of every confirmed finding from
# both adversarial review rounds (19 against the regex draft, 23 against the
# first parser); the allowed set includes every wrong-refusal those rounds
# confirmed. A vector that stops failing here has changed the guard's
# contract, not its implementation.
#
# Controls at the end: a neutered copy (the single refuse() exit flipped to
# allow) must let slip seven's vector through — proving the suite goes red
# when the guard is off — and a shimmed `git status` failure must refuse via
# the F-109 branch on a CLEAN tree, where no other branch can produce exit 2.
set -uo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
GUARD="$PWD/scripts/hooks/git_destructive_guard.sh"

T="$(mktemp -d "${TMPDIR:-/tmp}/nemr-guard-suite.XXXXXX")"
trap 'rm -rf "$T"' EXIT

mkrepo() { # $1 = path; creates repo with committed f.txt
    mkdir -p "$1" && cd "$1" || exit 1
    git init -q . && git config user.email x@x && git config user.name x
    echo base > f.txt && git add . && git commit -qm base
}

mkrepo "$T/dirty";  echo change >> "$T/dirty/f.txt"
mkrepo "$T/clean"
# deleted-but-tracked file (round-2 findings 15/16)
mkrepo "$T/deleted"; ( cd "$T/deleted" && echo x > del.txt && git add . && git commit -qm add && rm del.txt )
# branch/file name collision (round-2 finding 22) — dirty, so a false
# pathspec classification would refuse
mkrepo "$T/collide"; ( cd "$T/collide" && git branch foo && echo foo-file > foo && git add foo && git commit -qm foo && echo change >> f.txt )
# cumulative -C (round-2 finding 19): dirty repo at M/a/b, no M/b
mkdir -p "$T/M/a"; mkrepo "$T/M/a/b"; echo change >> "$T/M/a/b/f.txt"
# worktree with a dirty side worktree removable by basename (finding 21)
mkrepo "$T/wt-main"; ( cd "$T/wt-main" && git worktree add -q ../wt-side >/dev/null 2>&1 && echo change >> ../wt-side/f.txt )

payload() { python3 -c "import json,sys; print(json.dumps({'tool_name':'Bash','tool_input':{'command':sys.argv[1]},'cwd':sys.argv[2]}))" "$1" "$2"; }

RED=0; N=0
run() { # run <want> <label> <command> [cwd] [extra-env as VAR=val]
    local want="$1" label="$2" cmd="$3" dir="${4:-$T/dirty}" extra="${5:-}"
    N=$((N+1))
    if [ -n "$extra" ]; then
        payload "$cmd" "$dir" | env "$extra" bash "$GUARD" >/dev/null 2>"$T/err"
    else
        payload "$cmd" "$dir" | bash "$GUARD" >/dev/null 2>"$T/err"
    fi
    local got=$?
    if [ "$got" -ne "$want" ]; then
        RED=$((RED+1))
        printf 'RED  %-66s want=%s got=%s\n' "$label" "$want" "$got"
        sed 's/^/       /' "$T/err" | head -3
    fi
}

# --- destructive on a dirty tree -> refuse -------------------------------
run 2 "slip-7: git checkout -- f.txt"                    "git checkout -- f.txt"
run 2 "slip-3: git reset --hard"                         "git reset --hard"
run 2 "git reset -q --hard HEAD~1"                       "git reset -q --hard HEAD~1"
run 2 "git checkout ."                                   "git checkout ."
run 2 "git checkout -f main"                             "git checkout -f main"
run 2 "git checkout HEAD -- f.txt"                       "git checkout HEAD -- f.txt"
run 2 "git checkout f.txt (tracked pathspec, no --)"     "git checkout f.txt"
run 2 "git clean -fd"                                    "git clean -fd"
run 2 "git clean -d --force"                             "git clean -d --force"
run 2 "git restore f.txt (bare)"                         "git restore f.txt"
run 2 "git restore --staged --worktree f.txt"            "git restore --staged --worktree f.txt"
run 2 "git restore -SW f.txt"                            "git restore -SW f.txt"
run 2 "restore --staged a && restore b"                  "git restore --staged f.txt && git restore f.txt"
run 2 "restore --staged a & restore b (bg &)"            "git restore --staged f.txt & git restore f.txt"
run 2 "git switch -f b2"                                 "git switch -f b2"
run 2 "git switch -fq b2 (combined shorts)"              "git switch -fq b2"
run 2 "git switch --discard-changes b2"                  "git switch --discard-changes b2"
run 2 "git rm -f f.txt"                                  "git rm -f f.txt"
run 2 "git merge --abort"                                "git merge --abort"
run 2 "git rebase --abort"                               "git rebase --abort"
run 2 "git rebase main (plain)"                          "git rebase main"

# --- parsing shapes from both review rounds -> refuse --------------------
run 2 "compound: cd dirty && git reset --hard"           "cd $T/dirty && git reset --hard" "$T"
run 2 "quoted -C: git -C './dirty' reset --hard"         "git -C './dirty' reset --hard" "$T"
run 2 "variable -C fails closed"                         'git -C "$REPO" reset --hard' "$T"
run 2 "per-segment -C: dirty reset; clean status"        "git -C $T/dirty reset --hard; git -C $T/clean status" "$T"
run 2 "multiline: clean status NL dirty reset"           "$(printf 'git -C %s status\ngit -C %s reset --hard' "$T/clean" "$T/dirty")" "$T"
run 2 "no-C segment after -C segment"                    "git -C ../clean log; git reset --hard"
run 2 "backslash-newline continuation"                   "$(printf 'git \\\nreset --hard')"
run 2 "quoted pipe in checkout ref"                      "git checkout 'br|anch' -- f.txt"
run 2 "400-line -C bomb + final bare reset"              "$(python3 -c "print('\n'.join('git -C x%d reset --hard' % i for i in range(400)) + '\ngit reset --hard')")"
run 2 "override inside a string does not vouch"          "git commit -m 'NEMR_GIT_DESTRUCTIVE_OK=1 noted' && git reset --hard"
run 2 "two lines: status NL reset --hard"                "$(printf 'git status\ngit reset --hard')"
run 2 "comment swallows nothing: status #c NL reset"     "$(printf 'git status  # check first\ngit reset --hard')"
run 2 "comment on non-git line NL reset"                 "$(printf 'echo done  # cleanup\ngit reset --hard')"
run 2 "heredoc apostrophe body then reset"               "$(printf "cat > notes.md <<'EOF'\ndon't lose this\nEOF\ngit reset --hard")"
run 2 "heredoc cd body does not corrupt tracking"        "$(printf 'cat > run.sh <<EOF\ncd /nonexistent-dir-xyz\nEOF\ngit reset --hard')"
run 2 "failed cd with ; runs in OLD dirty cwd"           "cd /nonexistent-dir-xyz; git reset --hard"
run 2 "env -i git reset --hard"                          "env -i git reset --hard"
run 2 "sudo -u nemr git reset --hard"                    "sudo -u nemr git reset --hard"
run 2 "timeout -k 5 30 git reset --hard"                 "timeout -k 5 30 git reset --hard"
run 2 "timeout --signal=KILL 30 git reset --hard"        "timeout --signal=KILL 30 git reset --hard"
run 2 "time git reset --hard"                            "time git reset --hard"
run 2 "git --work-tree . reset --hard (space form)"      "git --work-tree . reset --hard"
run 2 "pushd other-dirty && git reset --hard"            "pushd $T/dirty && git reset --hard" "$T/clean"
run 2 "backtick substitution is seen"                    "echo \`git reset --hard\`"
run 2 "process substitution is seen"                     "cat <(git reset --hard)"
run 2 "leading fd redirection"                           "2>/dev/null git reset --hard"
run 2 "CDPATH makes relative cd unknowable"              "cd probe3 && git reset --hard" "$T/dirty" "CDPATH=$T"
run 2 "cumulative -C: git -C a -C b reset --hard"        "git -C a -C b reset --hard" "$T/M"
run 2 "checkout -- of deleted tracked file"              "git checkout -- del.txt" "$T/deleted"
run 2 "checkout of deleted tracked file (no --)"         "git checkout del.txt" "$T/deleted"
run 2 "abbreviated long option --har"                    "git reset --har"
run 2 "abbreviated --forc on clean"                      "git clean --forc -d"
run 2 "worktree remove -f by basename"                   "git worktree remove -f wt-side" "$T/wt-main"
run 2 "git stash -q drop (flag before action)"           "git stash -q drop"
run 2 "subshell (cd dirty && reset)"                     "(cd $T/dirty && git reset --hard)" "$T/clean"
run 2 "subshell pop then reset in dirty cwd"             "(cd $T/clean); git reset --hard"

# --- stash drop/clear: always refused (they destroy the remedy's product) -
run 2 "git stash drop (clean tree)"                      "git stash drop" "$T/clean"
run 2 "git stash clear (clean tree)"                     "git stash clear" "$T/clean"

# --- allowed: remedies, reads, carve-outs, mentions ----------------------
run 0 "git stash (the remedy)"                           "git stash"
run 0 "git stash push -m wip"                            "git stash push -m wip"
run 0 "git stash pop"                                    "git stash pop"
run 0 "git commit -am wip"                               "git commit -am wip"
run 0 "commit message mentioning reset --hard"           "git commit -am 'never run git reset --hard again'"
run 0 "commit message: fix: git reset --hard docs"       "git commit -am 'fix: git reset --hard docs'"
run 0 "heredoc that merely QUOTES the rule"              "$(printf "cat >> notes.md <<'EOF'\ngit reset --hard is forbidden by the rule\nEOF")"
run 0 "single line with trailing comment"                "git status  # check first"
run 0 "git add -A && git commit"                         "git add -A && git commit -m x"
run 0 "git status / log / diff"                          "git status; git log --oneline; git diff"
run 0 "git checkout -b branch"                           "git checkout -b wp-x"
run 0 "git checkout -bfix (stuck value, not -f)"         "git checkout -bfix"
run 0 "git checkout nonexistent-branch"                  "git checkout nonexistent-branch"
run 0 "checkout of branch colliding with a file"         "git checkout foo" "$T/collide"
run 0 "git restore --staged f.txt"                       "git restore --staged f.txt"
run 0 "git restore -S f.txt"                             "git restore -S f.txt"
run 0 "git rebase --continue (resolution flow)"          "git rebase --continue"
run 0 "git rm --cached f.txt"                            "git rm --cached f.txt"
run 0 "git clean -n (dry run)"                           "git clean -n"
run 0 "git reset --help is not --hard"                   "git reset --help"
run 0 "git reset -- --hard (a FILE named --hard)"        "git reset -- --hard"
run 0 "not git at all"                                   "cargo test --lib"
run 0 "digit trap"                                       "echo digital"
run 0 "2=x is not an assignment bash accepts"            "2=x git reset --hard"
run 0 "redirection then non-git"                         "git status > /dev/null; echo done"
run 0 "quoted newline stays one token"                   "$(printf "git commit -m 'line1\nline2 git reset --hard'")"

# --- declared destruction: env prefix of the invocation ------------------
run 0 "override on checkout --"                          "NEMR_GIT_DESTRUCTIVE_OK=1 git checkout -- f.txt"
run 0 "override on stash clear"                          "NEMR_GIT_DESTRUCTIVE_OK=1 git stash clear"

# --- clean tree / non-repo: precondition satisfied -----------------------
run 0 "git checkout -- f.txt (clean)"                    "git checkout -- f.txt" "$T/clean"
run 0 "git reset --hard (clean)"                         "git reset --hard" "$T/clean"
run 0 "git clean -fd (clean)"                            "git clean -fd" "$T/clean"
run 0 "git reset --hard in a non-repo"                   "git reset --hard" "$T"
run 0 "git -C /nonexistent reset --hard"                 "git -C /nonexistent-dir-xyz reset --hard" "$T"
run 0 "worktree remove of unknown name (git errors)"     "git worktree remove -f nosuch" "$T/clean"

# --- CONTROL 1: the neutered guard must let slip seven through -----------
sed 's/sys.exit(2)/sys.exit(0)/g' "$GUARD" > "$T/neutered.sh"
N=$((N+1))
payload "git checkout -- f.txt" "$T/dirty" | bash "$T/neutered.sh" >/dev/null 2>&1
if [ $? -ne 0 ]; then RED=$((RED+1)); echo "RED  CONTROL: neutered guard still refused — refusals not attributable to the guard"; fi

# --- CONTROL 2: unreadable state refuses via the F-109 branch on a CLEAN
# tree, where no other branch can produce exit 2, and names the failure ----
mkdir -p "$T/shim"
printf '#!/usr/bin/env bash\ncase "$*" in *rev-parse*) echo true; exit 0;; *status*) echo "fatal: index file corrupt" >&2; exit 128;; *) exec /usr/bin/git "$@";; esac\n' > "$T/shim/git"
chmod +x "$T/shim/git"
N=$((N+1))
payload "git reset --hard" "$T/clean" | PATH="$T/shim:$PATH" bash "$GUARD" >/dev/null 2>"$T/err"
if [ $? -ne 2 ] || ! grep -q "could not read" "$T/err"; then
    RED=$((RED+1)); echo "RED  CONTROL: F-109 branch did not refuse a failed read on a clean tree"
fi

echo "git guard suite: $N vectors, $RED red"
[ "$RED" -eq 0 ] || exit 1
echo "PASS"
