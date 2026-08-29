# Regression maintenance loop

Invoked ad hoc during engine work with `/loop 30m` (fixed interval) or a bare
`/loop` (self-paced), per SPEC 4A.6. Not run continuously outside active sessions.

## What runs, in order

1. `cargo test --lib` and `(cd deploy/nemr-volume && cargo test)` — unit tests,
   no host needed. Includes the octal-escape, `ModeTracker`, ioctl-struct-layout,
   kernel-version, and supervisor-trap guards.
2. `cargo clippy --all-targets` — must be warnings-clean (the CI lint gate).
3. `cargo test --test regression -- --test-threads=1` — host-backed regression
   suite. Refuses to run (does not skip) unless the host is provisioned and the
   **installed helper matches the built source** (TEST-01 hash gate).
4. `./scripts/e2e_smoke_test.sh` — full lifecycle, unprivileged.

`./scripts/verify_wp_a.sh` runs 1–4 in sequence.

## What counts as failure

- Any test failure, any clippy warning, any non-`PASS` from the smoke test.
- The hash gate firing (`installed helper does not match built source`) is a
  failure: it means a helper source change was not reinstalled. Fix by running
  `sudo ./scripts/setup_test_host.sh`, not by ignoring it.
- A smoke assertion on **host** state (loop device, mountinfo, backing file)
  failing while the engine reports success is the highest-priority class — it is
  the VOL-05 shape. Never downgrade it.

## The negative-assertion rule (a green signal over nothing)

The recurring defect that has caught this project four times is a green signal
over a non-working or non-existent artifact: VOL-05 reported success writing to
the wrong filesystem; the M7 branch was "done" with zero commits; the helper
passed 13 unit tests while unable to provision a volume; and a WP-C `grep`
"proved" history was not on the rootfs by searching `/var/lib/containerd`, the
**system** path, which under rootless is empty for a reason unrelated to the
claim.

**Standing rule: any "X is absent" / "not found" / "no matches" assertion must
first prove, in the same breath, that the search would have found a known-present
control.** Before trusting a negative:

- Prove the path is **readable and non-empty** — grep a control string you know
  is there, or list the directory and assert it has the expected entries.
- Under rootless, a path may be namespace-remapped; confirm you are searching the
  real host location (`~/.local/share/containerd/…`), not the system one.
- Never suppress errors (`2>/dev/null`) on the command whose *silence* you are
  about to read as evidence — a permission-denied is not an absence.

Absence-because-correct and absence-because-you-looked-in-the-wrong-place are
indistinguishable without the control. This recurs on every negative assertion.

## Tooling: two one-liners that misfired three times each, in one session

Not product findings. Recorded because three occurrences is where a rule stops
being something to remember and becomes something to write down — the same
argument that turned the wait discipline into `wait_for_service`.

**A pattern must not match the process running it.** `pkill -f "until grep -qE"`
killed its own shell. `pkill -f "sleep 45"` did it again. `pgrep -f "cargo test"`
matched the very script asking the question and aborted a mutation check with
"cargo is already running" — a **wrong reason that reads as a real one**, which
is the failure class this project keeps fixing in the product. Use `pgrep -x
<exe>`, or a pattern that cannot appear in the invoking command line. If you must
match loosely, exclude `$$` and the parent.

**Check the flag exists before putting it in a loop.** `gh run list --branch`,
`gh run list --json displayTitle` and `gh pr checks --json` were all invented —
each one plausible, none supported by this `gh`. The third sat inside a CI
watcher whose fallback was `|| echo '[]'`, so the failure was invisible and the
loop ran silently for fifteen minutes past the result. Run the command bare once
before building anything on it, and never wrap an unverified command in a
fallback that makes its failure indistinguishable from a negative answer.

The shape both share: reaching for a plausible one-liner instead of checking what
the tool actually supports, then discovering it only when the failure is silent
or destructive.

## Commit or stash before any destructive git operation

Fourth destructive slip in one session, at the shell, so this becomes a rule
rather than an acknowledgement — the same three-strikes argument that produced
`wait_for_service` and the pgrep rule above, one occurrence past it.

`git reset --hard origin/main` ran with four uncommitted CONFORMANCE entries in
the tree and deleted them. Mixed reset would have kept them; `--hard` was chosen
without looking at `git status` first. The same session also cleaned a working
tree with `git checkout -- .` and lost a saved-but-unapplied patch for long
enough to ship a circular verification on top of the gap.

**Before `reset --hard`, `checkout -- .`, `clean`, `rebase` or branch deletion:
run `git status --porcelain`, and commit or stash anything it shows.** A wip
commit costs nothing and reverts cleanly; there is no situation where losing the
tree is better.

The asymmetry is the reason the rule exists: the lost entries were recoverable
only because they had just been composed and could be rewritten from memory.
Code would not have been. **The rule exists for the case not yet hit** — the
one where the uncommitted work is hours of implementation rather than minutes of
prose.

## Protected subjects are enforced by name, not by convention

`htmltest` is irreplaceable, and "experiments use disposable subjects" was a
convention — which failed (F-123): a teardown check counted the running
project's link as a leak, and the cleanup that followed acted on a hardcoded
echo printed directly beneath a task listing that said RUNNING. The
contradicting evidence was on screen.

Now structural, in both harnesses, with a drift test tying them together:

- `NEMR_PROTECTED_SUBJECTS` in `scripts/lib/proc.sh` — every cleanup trap
  deletes through `delete_disposable`, and flow steps that run `nemr delete`
  visibly guard with `refuse_protected` first. The refusal happens BEFORE any
  command runs and is loud, because a silent skip would hide the bug that
  tried.
- `PROTECTED_SUBJECTS` in `tests/common/mod.rs` — `purge` refuses first,
  before acquiring any handle. Proven with a control: a disposable project
  temporarily marked protected survives purge; the same purge with protection
  lifted destroys it.

A protected subject must not depend on every future check being correct.

## Three ways a control fails, and three different remedies

They keep getting filed under one heading. They are not one problem, and the fix
for each is different.

**1. The assertion is written wrong.** It reads a value the defect also produces,
so it passes either way. *Remedy: fix the assertion.* Example: an "X is absent"
check that searched the system containerd path, which is empty under rootless for
an unrelated reason — the rule above.

**2. The control MUTATES what it observes.** It establishes its precondition by
calling something that repairs the state, so the code under test is handed an
already-correct subject and the guard stays green with the guarded code deleted.
*Remedy: make the control READ.* Example (F-112): an upgrade test established
"this container has no network namespace" by calling
`ensure_own_network_namespace`, which added one. `has_own_network_namespace`
exists solely so the control reads instead of repairs.

**3. The SUBJECT cannot exhibit the defect.** The assertion is right and the
control is honest, but no available subject differs from the correct case, so the
mutation passes and the test proves nothing. *Remedy: find or construct a subject
that can fail — and if you cannot, say so in the ledger rather than shipping a
guard that cannot bite.* Example (F-115/F-116): distinguishing "export reads the
container" from "export reads the engine constant" needs a project whose rootfs
differs from the constant, and every project the suite can create is built FROM
the constant, byte for byte.

**On #3, look harder before recording the gap.** F-116 was written as
unfalsifiable and was wrong: the missing ingredient was an older published base
image, kept permanently pullable by F-85 — a versioning discipline adopted for
bundle recoverability, which turned out to supply the fixture for an unrelated
test gap. The question to ask is not "can I construct this from what I am
holding" but "does anything this project already guarantees give me a subject
that differs". Recording an honest gap is a fine outcome; recording one that a
different part of the system already solves is a miss.

## The guard-test rule (a green signal over the *wrong* thing)

The negative-assertion rule is about a green signal over *nothing*. This one is
subtler: a green signal over the *wrong thing* — a test that exists, runs, and
passes, while guarding a property it does not actually enforce.

The reference case is F-56 (M9). `credentials_never_travel_under_any_policy`
passed on every run, but it asserted against `root/.claude/.credentials.json` —
the container's path — while a real export walks the *volume*, whose layout is
`.nemr-state/…`. The path it checked cannot occur, so the test guarded nothing.
D-02 was still true, but held **structurally** (the credential is a host
bind-mount that never reaches the volume), not because the test enforced it. The
test was false assurance sitting on top of a property that happened to be true
for unrelated reasons — invisible precisely because it was green.

**Standing rule: a guard test must be proven to fail when the guarded property
is violated.** If you can delete the code the test guards — the filter, the
check, the validation — and the test still passes, the test guards nothing.

- For any test whose name or intent is "X never happens" / "Y is always
  refused" / "Z cannot escape", confirm it goes red when X is made to happen.
  Write the violation, watch the test fail, then restore. This is test-before-fix
  applied to the *guard*, not just to the bug.
- Prefer asserting against **real, measured** inputs over synthetic ones at a
  boundary. F-56 and the `.claude.json` drift warning both passed every synthetic
  test and failed only against the layout/keys that actually occur.
- A guard whose property holds structurally (by construction elsewhere) is fine —
  but the test must still enforce it, so a future refactor that removes the
  structural guarantee turns the test red rather than leaving it falsely green.

Expect siblings: a defect of this shape is rarely alone. When one is found, audit
the other guard tests in the same pass.

## The count rule (a green suite that ran nothing)

`cargo test` exits **0 when it runs zero tests**. A mistyped name filter prints
`running 0 tests / test result: ok` and passes. A test that returns early is
counted as *passed*, so a suite can report "16 passed" while 14 of them did
nothing — which is exactly what CI's unit job did on every PR (F-60).

**Standing rule: a test run is evidence only if you know how many tests ran.**

- Any script or CI job that gates on a suite must assert the number that **ran**
  against the number that **exists** (`cargo test -- --list`), and that none
  skipped. Exit status alone is not evidence.
- Never gate on a name-filtered selection. If a filter is unavoidable, assert the
  expected count explicitly, because a renamed test silently selects nothing.
- An opt-out that makes tests return early (`NEMR_TEST_UNIT_ONLY`) must report
  how many it skipped. A job using one may claim only what it actually ran.

This is the same family as the negative-assertion and guard-test rules: the
signal is green, and the thing it is supposedly about never happened.

## The evidence rule (a failure that erased its own evidence)

The three rules above are about green signals. This one is about **red** ones,
and it came from three findings in a row that were the same defect wearing
different clothes:

- **F-61** — the bucket acceptance deleted the object it wrote on every exit
  path, so the only evidence of a run was the run's own summary.
- **F-65** — `output=$(cargo test ...)` under `bash -e` aborts at the
  *assignment*, so `status=$?` and every diagnostic below it were unreachable in
  all three places that run the suite, `verify_wp_a.sh` included. A red suite
  printed an exit code and nothing else.
- **F-66** — the offline test refused with "namespaces are unavailable" and did
  not say why, so the cause had to be inferred from outside CI.

In each case the *check* was correct and the *report* was worthless. That is
worse than a missing check, because a failure that says nothing gets attributed
to the last thing anyone touched.

**Standing rule: a failure must leave behind something a person can inspect
without re-running it, and the failure path must be executed at least once
before it is trusted.**

In practice:

- Print the underlying error, not a category. `unshare` says
  `write_setgroups failed: Permission denied`; "namespaces unavailable" does not.
- If a check consumes or deletes its evidence, give it an opt-out that keeps it
  (`--keep`) and print where the artifact went.
- Diagnostics are code. Run them — with a stand-in that fails on purpose — and
  assert the detail survives. `the_namespace_probe_reports_why_it_failed` is the
  shape.
- Under `set -e`, an assignment from a failing command aborts *there*. Put it in
  an `if` condition, or the handler beneath it is dead code.

## Purpose-built checks beat plausible one-liners (the control earns its keep)

Confirming the base image was reachable, three ad-hoc probes disagreed with each
other and with reality: a hand-run `gh api` said 403, a `curl` one-liner said
401, and `check_base_image_published.sh` correctly said it was fine. The
difference was the check's **control** — it first proves a known-public package
IS readable anonymously, so a 401/403 on our package can be told apart from
"anonymous GHCR access is broken from here / my token is stale / I got the
scope wrong." The one-liners had no control, so each failed for its own unrelated
reason and reported it as our package being unreachable.

The lesson, twice now (this and F-80): a check without a self-contained control
can fail — or pass — for reasons unrelated to the property it guards, and a
plausible one-liner is exactly the thing that looks authoritative while doing
so. When the answer matters, reach for the checked tool, not the quick probe;
and every checker this project ships carries a control that would fail if the
check were testing nothing.

## The cleanup rule (a failure that destroyed its own evidence)

The evidence rule says a failure must leave something inspectable. Cleanup paths
are where that goes wrong worst, and F-79 is the case:

- The test sweep called `unmount_and_detach`, got `Ok`, and deleted the image
  and the mount point. The helper had silently failed to detach (F-77), so the
  loop device stayed — and the two things `nemr reconcile` enumerates were now
  gone. 57 devices holding 24 GB became invisible, and `reconcile` answered
  *"nothing to reconcile"*.
- It was committed in the same pass that fixed assume-success in `reconcile`'s
  own reporting. Knowing the principle did not stop me applying its opposite one
  function away.

**Standing rule: a cleanup path must verify the release happened before deleting
anything that records what needs releasing.**

Assume-success is bad everywhere. In a cleanup path it is worse than elsewhere,
because **the assumption deletes the evidence** — the failure and the trail to
it are destroyed by the same call. Nothing is left to notice, and the next
command that looks reports all-clear.

In practice:

- Observe, then destroy. Never destroy on the strength of a call returning `Ok`.
- If the release did not happen, **leave the artifacts in place** and say so. A
  visible mess is recoverable; an invisible one is not.
- Two commands that read the same host state must share an enumeration. `list`
  reading `/sys` while `reconcile` read directories is how one reported 57 and
  the other reported none, both truthfully.
- A cleanup command's false all-clear is worse than a noisy one: it ends the
  investigation.

## The pre-push rule (a check CI runs is a hook, not a memory)

Twice in WP-J the same lapse shipped: code that was `cargo clippy`-checked but
not `cargo fmt`-checked, pushed, and rejected by CI's lint job only after a
round-trip. Naming it twice and resolving to "remember fmt next time" is exactly
the wrong fix — a discipline that lives in memory fails the moment attention is
elsewhere, which is when it matters.

The fix is structural: `scripts/git-hooks/pre-push` runs `cargo fmt --all --
--check` (both workspaces) and blocks the push if the tree is not clean;
`scripts/setup_host.sh` installs it via `git config core.hooksPath
scripts/git-hooks`. `cargo fmt --check` only parses, so it is cheap enough to run
on every push.

General rule: **any fast check CI performs should run locally before the push,
enforced by a hook rather than a habit.** fmt is the first; clippy is too slow
for a pre-push gate, but if a second cheap check earns a CI job, add it to the
hook in the same breath.

**The same lesson, applied to waiting (F-95).** Seven wait loops shipped
unable to explain their own failures, and the seventh was written *in a script
authored after this file already said not to*. That is the evidence that a rule
here competes with the moment of writing and loses. Waiting now lives in
`scripts/lib/proc.sh` (`wait_for_service`, `wait_for_ready`, `require_tcp`) and
`scripts/check_wait_discipline.sh` fails the build when a script backgrounds a
process without it. **When a rule in this file recurs, the question is not how
to remember it harder — it is what would make forgetting impossible.**

## Fix autonomously

- A newly `#[ignore]`d or skipped test — de-skip it and make it run.
- A clippy warning — fix it.
- A flaky assertion caused by leftover host state — the regression harness's
  `TestProject` purges on drop; if residue appears, run `nemr reconcile` and
  harden the test's cleanup.
- A smoke-test timing/ordering assumption — de-brittle it (no sleeps, no
  reliance on prior-run state).
- A genuine regression whose fix is local and provable with a test — fix it
  test-first and commit both.

## Bring to Rain

- Any failure that can only be resolved by changing SPEC.md Sections 1–3, or by
  widening the privileged helper's scope (PRIV-05). Escalate per Section 9.
- Any new instance of a silent-success/wrong-result defect (VOL-05 class) —
  fix it, but flag it, because a second one means the class is not contained.
- Any new instance of a failure that erased its own evidence (F-65 class) —
  same reasoning, and it hides the VOL-05 ones.
- Any cleanup path that destroys before verifying (F-79 class) — it hides both.
- A regression whose only fix trades off against a Section-7 gate (E-01…E-11).
