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
