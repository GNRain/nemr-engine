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
