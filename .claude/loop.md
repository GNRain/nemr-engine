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
- A regression whose only fix trades off against a Section-7 gate (E-01…E-11).
