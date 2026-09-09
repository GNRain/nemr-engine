# Gate audit — what is not watching

**Asked for by the Product Owner, 2026-09-09:** *"List every script and every
assertion in this repo that nothing currently executes, or whose failure no gate
would notice. I want the size of the class before deciding what to do about
it."*

**What provoked it.** `scripts/e2e_smoke_test.sh` asserted that the credential
bind was read-write by grepping `/proc/self/mountinfo` for
`" /root/.claude/.credentials.json "`. F-14 moved that bind from the file to the
directory. The assertion was red from that moment and nobody saw it, because
nothing ran the script. It surfaced only when `scripts/install.sh` chose to
verify with it (D-14).

This document is a **measurement, not a plan**. Nothing here is fixed.

---

## 1. The size of the class

| | Count | How it was established |
|---|---|---|
| Shell/Python scripts in the repo | **40** (+ `pre-push`) | `find` over the tree, minus `target/` |
| …that **nothing executes today** | **24** | caller graph over `*.sh`, `*.rs`, `*.yml`, `*.py`, `*.md`, plus the two workflows |
| …that have **no caller in any state of the world** (not even a dead CI job) | **8** | of which 2 are entry points by design (`setup_host.sh`, `install_server.sh`) |
| Acceptance scripts that **assert their own assertion count** | **2 of 8** | `test_install.sh`, `docs/ui-acceptance.sh`; six others print a tally and pass |
| Regression tests that **skip under `NEMR_TEST_UNIT_ONLY`** and are counted as passed | **33 of 76** | measured: `NEMR_TEST_UNIT_ONLY=1 cargo test --test regression -- --nocapture` |
| Unit tests outside the aggregator a human is told to run | **60 of 220** | `cargo test --lib` = 160; `cargo test --workspace --lib` = 220 |
| Distinct findings after dedupe (six sweeps) | **107** | 29 high, 55 medium, 23 low |

**One cause dominates.** `.github/workflows/ci.yml` is the only automatic gate
this repo has, and it has not executed a single step since **2026-08-29
15:33:59Z** — 11 days, **125 commits on `main`, 43 merged PRs, 59 SPEC rows**
ago. Every run since is created and instantly fails with
`steps: 0` and the annotation *"The job was not started because recent account
payments have failed or your spending limit needs to be increased."* That one
dead file is what makes 21 script invocations and 8 `cargo test` commands
unexecuted, including the E2E smoke test that started this.

**What still fires without a human deciding to:**

1. `scripts/git-hooks/pre-push` — `cargo fmt --check` on two workspaces. Nothing else.
2. The Claude Code `PreToolUse` Bash guard (`scripts/hooks/git_destructive_guard.sh`) — only inside an agent session.

Everything else in this repository — every seam check, every acceptance, every
smoke test, clippy, the whole regression suite — runs when, and only when, a
person remembers to type it.

---

## 2. Class A — nothing executes it

### 2.1 No caller of any kind

| Script | Size | What it proves, and for whom |
|---|---|---|
| `docs/ui-acceptance.sh` + `docs/ui-acceptance.py` | 836 + ~790 lines | The whole GUI half — register, login, credential step, create, add, attach, push, pull, cloud delete — **99 assertions (103 with S3)**. Cited as the proof in eight SPEC rows (E-19, E-20, E-21, F-11, F-12, F-13, E-23, F-20). No script, workflow, test or routine document invokes it. |
| `scripts/test_install.sh` | 265 lines | The D-14 installer acceptance, 39 asserted assertions. Written today; already has no invoker. |
| `docs/credential-acceptance.sh` | 36 lines | D-02 (f)/E6 — a running session survives a host credential rotation. **Also stale** (§3.5). |
| `docs/first-run-acceptance.sh` | 61 lines | F-131 — a never-used session opens straight at the prompt. **Also stale** (§3.5). |
| `docs/gpu-env.sh` | 86 lines | The E-18 GPU contract, by design a spike artefact. |
| `scripts/setup_host.sh`, `scripts/install_server.sh` | 495 + 252 | Entry points **by design** — a human types them. Listed for completeness. |

### 2.2 Executed only by the dead workflow

Every one of these has exactly one caller, and it is `ci.yml` or
`publish-base-image.yml`:

`check_docker_free.sh` (NFR-01), `check_base_image_pins.sh` (F-74),
`check_base_image_versioning.sh` (F-85), `check_cli_seam.sh` (E-09),
`check_seam.sh` (E-11), `check_wait_discipline.sh` (F-95), `test_proc_lib.sh`,
`test_fetch_base_image.sh`, `check_base_image_reproducible.sh`,
`ci_provision_host.sh`, `port_acceptance.sh` (WP-M), `netns_acceptance.sh`
(NET-02/05), `packages_acceptance.sh` (F-118), `sync_acceptance.sh` (WP-J/K),
`publish_base_image.sh`, `check_base_image_published.sh` (D-08), and
`e2e_smoke_test.sh` (AC-7.2).

Plus the `cargo` commands only CI runs: `cargo test -p nemr-sync`,
`cargo test -p nemr-cloud` (the sync server's and client's integration suites),
`cargo clippy` on both workspaces, and `cargo deny`.

### 2.3 Rust that no command reaches

- **`src/bin/nemr.rs:1620`** — `a_folder_name_becomes_a_name_the_engine_accepts`,
  the only test in the `nemr` binary target. `--workspace --lib` excludes bin
  targets; `verify_wp_a.sh` uses `--lib`; no routine uses `--bins` or
  `--all-targets`. Verified: `cargo test --bin nemr -- --list` → 1 test.
- **60 of 220 unit tests** are outside `verify_wp_a.sh`, which runs
  `cargo test --lib` (the root package only, 160 tests) rather than
  `--workspace --lib` (220). The missing 60 are the containerd wrapper, crypto,
  storage, sync and daemon-api crates. `README.md` does say `--workspace --lib`;
  the aggregator does not.
- **`crates/nemr-storage/src/bin/bucket_roundtrip.rs`** and the four other
  committed diagnostic binaries — the only live-bucket conformance path.

---

## 3. Class B — it runs, but its failure would not be noticed

### 3.1 Suites that can pass over nothing

Six of eight acceptance scripts print a tally and exit 0 — a skipped step is
invisible: `e2e_smoke_test.sh`, `sync_acceptance.sh`, `netns_acceptance.sh`,
`port_acceptance.sh`, `packages_acceptance.sh`, `test_proc_lib.sh`. Only
`docs/ui-acceptance.sh` and `scripts/test_install.sh` assert the count (F-6).

CI's own accounting is weaker than it reads: the unit job computes
`ran = total - skipped` and then asserts only `ran > 0`, so 75 of 76 tests
skipping would still be green.

### 3.2 Silent skips

- **33 of 76 regression tests** return early under `NEMR_TEST_UNIT_ONLY` and are
  **counted as passed** by cargo. The seam prints a line, which only a
  `--nocapture` reader sees.
- `NEMR_SKIP_API=1` disables the live API round-trip in `e2e_smoke_test.sh`,
  `netns_acceptance.sh` and `sync_acceptance.sh` — and CI always sets it.
- `NEMR_HUMAN_LOGIN=1` (the E-21 human arm, +4 assertions, including the
  Product Owner's REQUIRED PROOF) is set by nothing.
- Seams with no setter anywhere in the repo: `NEMR_TEST_FORCE_STATUS`,
  `NEMR_TEST_FORCE_DIGEST`, `NEMR_EXPECT_BASE_DIGEST`, `NEMR_TEST_BUILDER`.

### 3.3 Assertions that cannot fail as written

- **`scripts/sync_acceptance.sh:172`** — E-16's "the server holds only
  ciphertext" is checked **only when `NEMR_SKIP_API=1`**; in the full-API mode
  the `pass "the stored object is ciphertext"` prints without any check.
- **`scripts/e2e_smoke_test.sh:120`** — the F-62 freshness gate runs
  `cargo test --test regression the_installed_engine_matches_its_source --quiet`.
  A zero-match filter exits **0** (verified). Rename that test and the gate is
  silently disarmed.

### 3.4 Failures the shell throws away

25 sites where a check's status cannot fail the run (`|| true` on an assertion
rather than a probe, unchecked pipelines, `set +e` outside a trap). The sweep
separated these from the legitimate uses (probes whose failure IS the
measurement, and cleanup traps) and did not report the latter.

### 3.5 Stale subjects — the AUTH-02 class, still live

| Where | What it still names | Since |
|---|---|---|
| `tests/common/mod.rs:1108` | `run_offline()` mounts a tmpfs over `$HOME/.claude` to establish "no credential". The credential moved to `$HOME/.local/share/nemr/host-credential` — **the tmpfs no longer hides it**, so `e11_export_and_import_work_with_no_network_and_no_credentials` proves the network half only. It passes either way. | F-14 |
| `scripts/setup_host.sh:438` | Checks `~/.claude/.credentials.json` and tells the user to run `claude` on the host — **the exact advice F-24 removed**. The F-24 guard test does not cover this file. | F-14 / F-24 |
| `scripts/ci_provision_host.sh:127` | Writes a placeholder credential at `~/.claude/.credentials.json`, which the engine no longer reads. | F-14 |
| `docs/credential-acceptance.sh:14`, `docs/first-run-acceptance.sh:23` | Both read `~/.claude/.credentials.json` as the machine's nemr login. | F-14 |

### 3.6 Documentation that describes a gate that does not exist

`.claude/loop.md:17` — *"`./scripts/verify_wp_a.sh` runs 1–4 in sequence"*, where
step 2 is clippy. `verify_wp_a.sh` contains no clippy invocation. Clippy's only
gate is the dead CI job.

---

## 4. Confidence

**Verified by direct measurement in this session** (numbers above): the CI
blackout and its window, the 33/76 skip count, `--lib` 160 vs
`--workspace --lib` 220, the orphaned bin test, the zero-match `cargo test` exit
status, the count-assertion inventory, every entry in §3.5, and §3.6.

**From the sweep, not independently re-verified:** the remaining detail of the
107 findings. Six parallel sweeps produced 139 raw findings (107 after dedupe);
an adversarial verification pass was planned for all of them and **67 of its 90
agents died on a session limit**, so only the `orphan-scripts` dimension was
fully double-checked. The per-finding appendix below is therefore a
*candidate list* except where §4 paragraph 1 says otherwise.

**Not looked at:** the completeness critic (the agent that asks what the sweep
missed) also died on the session limit. Known unswept ground: the Rust
`#[cfg(test)]` bodies were read only for skip-shaped patterns, not for weak
assertions generally; `deploy/nemr-volume`'s own test suite; and the
`.github/workflows/publish-base-image.yml` job body beyond its script calls.

---

## 5. Appendix — the 107 findings

Severity as assigned by the sweep. `dim` names the sweep that found it.

| Sev | Path:line | Kind | What it claims to check |
|---|---|---|---|
| high | `.github/workflows/ci.yml:0` | disabled-gate | The entire CI board: NFR-01 Docker-freeness, both seams, wait discipline, base-image pins/versioning, clippy+fmt, unit tests, the regression suite, su |
| high | `.github/workflows/ci.yml:1` | not-executed | Every gate in ci.yml (Docker-freeness, base-image pins/versioning, E-09 and E-11 seams, wait discipline, clippy, fmt, unit tests, cargo-deny, sync tes |
| high | `.github/workflows/ci.yml:19` | disabled-gate | Every mechanical gate in the repo: 8 static seam/discipline checks, clippy+fmt on both workspaces, the workspace unit tests, cargo-deny, the sync-serv |
| high | `.github/workflows/ci.yml:33` | not-executed | Six static seam and discipline gates whose ONLY caller is the dead workflow: check_docker_free.sh (:33, NFR-01), check_base_image_pins.sh (:36), check |
| high | `.github/workflows/ci.yml:114` | uncounted | The unit-tests job computes ran=total-skipped by grepping for the skip message and publishes '::notice::host-free coverage: $ran of $total regression  |
| high | `.github/workflows/ci.yml:268` | not-executed | scripts/netns_acceptance.sh — the NET-02 network-namespace acceptance (per-project netns, DNS, egress, port isolation across two ports). |
| high | `.github/workflows/ci.yml:287` | not-executed | scripts/sync_acceptance.sh — the WP-J sync acceptance: register, login, push, pull, release, and transcript byte-fidelity against a live nemr-sync + P |
| high | `docs/ENGINEERING.md:217` | vacuous-assertion | AC-1.2 — the containerd wrapper's output must be byte-identical to the raw baseline's. The documented acceptance is `diff <(raw_connectivity 2>/dev/nu |
| high | `docs/credential-acceptance.sh:1` | not-executed | The credential acceptance (D-02 (f), E6): whether a RUNNING session's Claude Code still answers after the host token expired and the host refreshed. E |
| high | `docs/credential-acceptance.sh:14` | stale-subject | cred="$HOME/.claude/.credentials.json"; the script then prints that file's access-token expiry and refresh window as "host:" state, and the whole acce |
| high | `docs/first-run-acceptance.sh:1` | not-executed | The first-run acceptance: drives an interactive `claude` under a pty inside a never-used session and asserts theme picker=0, login method=0, trust dia |
| high | `docs/first-run-acceptance.sh:23` | stale-subject | Line 23-24 asserts the host credential exists by reading ~/.claude/.credentials.json, and exits 1 with "no host credential; log in on the host first"  |
| high | `docs/ui-acceptance.sh:0` | not-executed | The full browser acceptance for the GUI half — 836 shell lines plus 792 Python lines driving real Firefox over BiDi — covering E-19, E-20 (both local  |
| high | `docs/ui-acceptance.sh:1` | not-executed | 836-line browser acceptance for the whole GUI (E-19/E-20/E-21): register, login, credential step, project create, bundle push/pull, S3 round-trip, del |
| high | `docs/ui-acceptance.sh:24` | not-executed | The browser acceptance: 99 assertions (103 with S3), driving real Firefox over BiDi through login, list, pull, attach, push, stop, the credential step |
| high | `docs/ui-acceptance.sh:529` | stale-subject | Claims that after the E-21 credential seam is removed, a clean daemon no longer reports the seam state — i.e. `nemr status $PROJECT` must NOT say 'NO  |
| high | `scripts/check_wait_discipline.sh:62` | disabled-gate | F-95: "every script that backgrounds a process waits with wait_for_service" — the gate that exists because the rule lost seven times to the moment of  |
| high | `scripts/e2e_smoke_test.sh:121` | vacuous-assertion | The F-62 freshness gate — "the installed nemr is this source" — implemented as `cargo test --test regression the_installed_engine_matches_its_source - |
| high | `scripts/e2e_smoke_test.sh:239` | silent-skip | That Claude Code inside a session can complete a live API round-trip (credentials + network egress together) — the strongest claim in the smoke test. |
| high | `scripts/e2e_smoke_test.sh:394` | uncounted | The standing regression test for all engine changes (AC-7.2) — the exact script whose credential-bind assertion was red for days. Its verdict is `prin |
| high | `scripts/hooks/git_destructive_guard.sh:74` | disabled-gate | That no destructive git command (reset --hard, checkout --, clean -fd, stash drop/clear, worktree remove -f …) runs while the tree has uncommitted wor |
| high | `scripts/sync_acceptance.sh:172` | vacuous-assertion | E-16: the object the sync server stores must be ciphertext, not the session's plaintext. |
| high | `scripts/sync_acceptance.sh:175` | vacuous-assertion | Claims E-16: 'the stored object is ciphertext' — the server must never hold plaintext session bytes. |
| high | `scripts/test_install.sh:0` | not-executed | A 265-line, 39-assertion acceptance suite for scripts/install.sh and scripts/lib/cat.sh — every preflight refusal by name, the plan's completeness, th |
| high | `scripts/test_install.sh:1` | not-executed | The installer acceptance, 265 lines / 37 asserted assertions: every preflight refusal by name, the plan naming every sudo command extracted from the s |
| high | `scripts/test_install.sh:31` | not-executed | The installer acceptance: 39 assertions covering every preflight refusal by name, the plan's completeness, the consent rule in both directions, idempo |
| high | `scripts/verify_wp_a.sh:38` | not-executed | `cargo test --lib --quiet` in the one aggregator a human is told to run for acceptance. It is meant to be the unit-test arm (README.md:344 and ci.yml: |
| high | `tests/common/mod.rs:1108` | stale-subject | run_offline() claims to run the CLI with "no network and no credential" by mounting a tmpfs over $HOME/.claude — the seam behind E-11's offline guaran |
| high | `tests/regression.rs:53` | silent-skip | 27 of the 76 regression tests (NET-02 wiring, NET-05 isolation, all five F-14 credential-bind tests, E-21 placeholder login, four F-115 base-image tes |
| medium | `.github/workflows/ci.yml:46` | not-executed | scripts/test_proc_lib.sh — the suite for scripts/lib/proc.sh (wait_for_service, wait_for_ready, require_tcp, refuse_protected), the library sourced by |
| medium | `.github/workflows/ci.yml:48` | not-executed | scripts/test_fetch_base_image.sh — the offline test suite for fetch_base_image.sh, including the neutered-copy CONTROL that proves the digest refusals |
| medium | `.github/workflows/ci.yml:85` | uncounted | Four cargo test invocations gate CI on suites without asserting how many tests ran: `cargo test --workspace --lib` (line 85), `cd deploy/nemr-volume & |
| medium | `.github/workflows/ci.yml:112` | uncounted | The unit-tests job's coverage accounting: `skipped=$(grep -c 'skipping a host-backed regression test')` and `ran=$(( total - skipped ))`, reported as  |
| medium | `.github/workflows/ci.yml:166` | not-executed | `cargo test -p nemr-sync` — the sync server's integration suites crates/nemr-sync/tests/{identity,lease,bundles}.rs (10/6/3 tests) against Postgres. |
| medium | `.github/workflows/ci.yml:175` | not-executed | `cargo test -p nemr-cloud` — the commercial client's 11 CLI integration tests (crates/nemr-cloud/tests/cli.rs) plus its src/core.rs and src/serve.rs u |
| medium | `.github/workflows/ci.yml:213` | not-executed | scripts/check_base_image_reproducible.sh — F-74, that the base image rebuilds to the recorded digest. |
| medium | `.github/workflows/ci.yml:261` | not-executed | scripts/port_acceptance.sh — the host-port publish/isolation acceptance. |
| medium | `.github/workflows/ci.yml:276` | not-executed | scripts/packages_acceptance.sh — the in-session package-installation acceptance. |
| medium | `.github/workflows/publish-base-image.yml:14` | not-executed | check_base_image_published.sh — the gate that the base image is obtainable WITHOUT a GHCR account, and that the published digest has not diverged from |
| medium | `.github/workflows/publish-base-image.yml:99` | not-executed | scripts/publish_base_image.sh (:90) and scripts/check_base_image_published.sh (:99) — publishing the base image at a verified digest, and proving it i |
| medium | `crates/nemr-containerd/src/bin/raw_connectivity.rs:0` | not-executed | Five committed diagnostic/evidence binaries: raw_connectivity and wrapper_connectivity (the AC-1.2 baseline pair — wrapper output must be identical to |
| medium | `crates/nemr-storage/src/bin/bucket_roundtrip.rs:141` | not-executed | The live-bucket acceptance for the S3/R2 backend — the only code path that exercises the ObjectStore conformance suite against a real object store. |
| medium | `docs/ENGINEERING.md:445` | vacuous-assertion | A documented developer routine: `cargo test --lib -- --ignored --nocapture --test-threads=1   # 4 integration`, introduced by "The integration tests m |
| medium | `docs/credential-acceptance.sh:6` | not-executed | D-02 (f) / E6: a running session's Claude Code still answers after the host's access token expired and the host refreshed underneath it. |
| medium | `docs/first-run-acceptance.sh:9` | not-executed | F-131: on a never-used session, `claude` opens straight at the prompt — no theme picker, no login-method screen, no trust dialog. It drives an interac |
| medium | `docs/ui-acceptance.py:1` | not-executed | The browser driver behind the UI acceptance — page interaction over BiDi plus the S3 verbs (s3-get, s3-list, s3-delete-prefix) used to prove bundle by |
| medium | `docs/ui-acceptance.sh:48` | silent-skip | The E-20 object-store arm: with NEMR_S3_BUCKET set, ui-acceptance drives the sync server against a real bucket and reads the bucket back independently |
| medium | `docs/ui-acceptance.sh:49` | silent-skip | EXPECTED_ASSERTIONS — the F-6 count gate that is supposed to catch a run which skipped a step. |
| medium | `docs/ui-acceptance.sh:501` | vacuous-assertion | The CONTROL that a placeholder is not a login: `grep -qi 'not logged in\\|login\\|authenticate' <<<"$attach_out"` where $attach_out is `nemr attach "$ |
| medium | `docs/ui-acceptance.sh:555` | not-executed | The NEMR_HUMAN_LOGIN=1 arm — the E-21 human credential step on a host with no Claude login, worth +4 of the script's asserted assertions (:54 bumps EX |
| medium | `scripts/check_base_image_published.sh:19` | not-executed | The D-08 obtainability guarantee: every version in image/digests/ is pullable anonymously from ghcr.io at exactly the recorded bytes — the check that  |
| medium | `scripts/check_base_image_published.sh:100` | not-executed | The F-86/F-87 test seams NEMR_TEST_FORCE_STATUS / NEMR_TEST_FORCE_DIGEST, whose comment claims they exist "so the branches are provable without a live |
| medium | `scripts/check_cli_seam.sh:24` | vacuous-assertion | E-09: the CLI (src/bin/nemr.rs) must have no direct containerd path; every command goes through the daemon. |
| medium | `scripts/check_docker_free.sh:89` | stale-subject | NFR-01 section 3: `ok "no Docker build/run command in docs or scripts"` — the gate asserts that no Docker build/run command is documented. |
| medium | `scripts/check_wait_discipline.sh:39` | silent-skip | "no script rolls its own readiness loop" (F-95) — scoped by `find scripts -maxdepth 2 -name '*.sh'`, with the predicate "the file captures $! AND ment |
| medium | `scripts/ci_provision_host.sh:128` | stale-subject | A CI provisioning step that writes a placeholder Claude credential to $HOME/.claude/.credentials.json, justified by "AUTH-03 makes a missing host cred |
| medium | `scripts/e2e_smoke_test.sh:120` | silent-skip | The F-62 freshness gate: the smoke test refuses to run against a nemr binary that is not this source, by shelling out to the regression test the_insta |
| medium | `scripts/e2e_smoke_test.sh:363` | vacuous-assertion | 'snapshot removed' — after `nemr delete`, containerd must hold no snapshot for the project. |
| medium | `scripts/fetch_base_image.sh:48` | not-executed | BUILDER="${NEMR_TEST_BUILDER:-./scripts/build_base_image.sh}" — the seam whose comment reads "the suite substitutes a shim builder so every branch is  |
| medium | `scripts/install_engine.sh:42` | vacuous-assertion | That the nemr binary installed to ~/.local/bin is byte-identical to the one just built — the freshness discipline the whole test estate depends on (F- |
| medium | `scripts/lib/proc.sh:190` | disabled-gate | F-123's protected-subject guard: refuse_protected()/purge_protected() in shell and is_protected() in Rust are meant to stop any automated path deletin |
| medium | `scripts/lib/proc.sh:197` | vacuous-assertion | `refuse_protected` (and `delete_disposable` through it) — the F-123 guard that no automated path may ever delete a protected project. |
| medium | `scripts/netns_acceptance.sh:194` | vacuous-assertion | The NET-02 live API round-trip — sessions can actually reach api.anthropic.com through the NAT. |
| medium | `scripts/netns_acceptance.sh:296` | uncounted | NET-02 acceptance — two sessions on the same internal port, each in its own netns, with a control proving the NAT provides egress. |
| medium | `scripts/packages_acceptance.sh:140` | uncounted | F-118 acceptance — a session's installed tools travel with it: install jq, export, import clean, prove jq absent (the pristine-import control), provis |
| medium | `scripts/port_acceptance.sh:69` | vacuous-assertion | 'a server is listening on 8000 inside the session' — the node HTTP server started by the previous line is up. |
| medium | `scripts/port_acceptance.sh:97` | vacuous-assertion | That `nemr stop` releases the host port, and (line 115) that `nemr delete` releases it — the two teardown guarantees of WP-M. |
| medium | `scripts/port_acceptance.sh:120` | uncounted | WP-M port forwarding acceptance — a session's dev server is reachable from the host, with a control proving the port was refused beforehand. |
| medium | `scripts/setup_host.sh:438` | stale-subject | The developer provisioner's credential step: `if [[ -e "$HOME/.claude/.credentials.json" ]]` … else print "No credential at ~/.claude/.credentials.jso |
| medium | `scripts/setup_host.sh:459` | discarded-status | That the fmt pre-push guard (scripts/git-hooks/pre-push) is installed and will actually run. |
| medium | `scripts/setup_test_host.sh:103` | discarded-status | A step headed '==> Verifying' that claims to verify the freshly installed privileged helper and its sudoers grant. |
| medium | `scripts/setup_test_host.sh:104` | discarded-status | The step titled "Verifying" — that the privileged helper installed at /usr/local/libexec/nemr-volume actually runs, and that the sudoers grant landed  |
| medium | `scripts/sync_acceptance.sh:204` | vacuous-assertion | "session state is byte-identical through encrypt->push->delete->pull->import" — the round-trip fidelity claim. |
| medium | `scripts/sync_acceptance.sh:246` | uncounted | The WP-K product flow — register, create, work, push, DELETE THE PROJECT, pull, continue. |
| medium | `scripts/test_fetch_base_image.sh:173` | vacuous-assertion | A CONTROL claiming 'neutering changed nothing — the refusal site moved': after `sed 's/exit 1/exit 0/g'` the neutered copy must contain at least one ` |
| medium | `scripts/test_git_guard.sh:221` | uncounted | The git destructive-guard acceptance: "every reproducing input from BOTH adversarial review rounds — 19 confirmed breaks against the regex draft, 23 a |
| medium | `scripts/test_install.sh:217` | silent-skip | "an interrupt gives the cursor back" and "an interrupt leaves no drawing process behind" — the two assertions that the install animation cleans up whe |
| medium | `scripts/test_install.sh:229` | vacuous-assertion | Claims 'an interrupt leaves no drawing process behind' — after SIGINT, no cat-animation process may survive. |
| medium | `scripts/verify_wp_a.sh:9` | not-executed | The strict regression gate: helper freshness (TEST-01 hash), unit tests, the full 76-test regression suite with `passed == total && skipped == 0`, and |
| medium | `src/bin/nemr.rs:1620` | not-executed | tests::a_folder_name_becomes_a_name_the_engine_accepts — the only test in the nemr binary target, covering folder-name → project-name normalisation on |
| medium | `src/bin/nemr.rs:1628` | not-executed | `a_folder_name_becomes_a_name_the_engine_accepts` — the F-20/F-21 guard that `nemr add <dir>` derives a session name the engine will actually accept ( |
| medium | `tests/regression.rs:747` | vacuous-assertion | D-02, the strongest half of `m9_a_real_bundle_contains_no_credential`: the host's real credential contents must not appear in an exported bundle. |
| medium | `tests/regression.rs:4480` | vacuous-assertion | f123_the_shell_and_rust_protected_lists_agree asserts 'the shell guard and the Rust guard protect different sets — the rule has forked' can never happ |
| medium | `tests/regression.rs:4491` | vacuous-assertion | `f123_the_shell_and_rust_protected_lists_agree` — the shell guard and the Rust guard must protect the same set of project names. |
| low | `.github/workflows/ci.yml:200` | not-executed | scripts/ci_provision_host.sh — provisions the clean-runner Docker-free stack (disables the system containerd, installs the rootless stack, pulls the b |
| low | `crates/nemr-storage/src/s3.rs:449` | silent-skip | an_unconfigured_environment_yields_none_not_an_error asserts that the open engine never requires a storage backend: S3Config::from_env() must be Ok(No |
| low | `docs/gpu-env.sh:0` | not-executed | The GPU spike's sourceable shell definitions (86 lines) for the by-hand E-18 investigation. |
| low | `docs/gpu-env.sh:1` | not-executed | The GPU spike's environment contract — the driver binds, CTRUN wrapper, CLAUDE_TASK/AGENT_ENV, and the self-check that "must print nothing else". |
| low | `docs/gpu-env.sh:3` | not-executed | The GPU spike's shell definitions (CTRUN, driver-store binds, AGENT_ENV) — the contract E-18 was measured against. |
| low | `docs/gpu-env.sh:86` | discarded-status | The file's own "self-check": that every container run in the GPU runbooks goes through CTRUN (so the rootless cgroup flags are never omitted). |
| low | `docs/ui-acceptance.py:531` | vacuous-assertion | F-21's "the add panel has no typed-name gate": `not await b.eval("!!document.getElementById('addtypeit')")`. |
| low | `scripts/build_base_image.sh:153` | not-executed | The NEMR_EXPECT_BASE_DIGEST arm: if set, compare the built digest against it and exit 1 on mismatch. |
| low | `scripts/check_base_image_published.sh:130` | vacuous-assertion | The F-85 immutability check: the digest GHCR serves for a version must equal the digest recorded in image/digests/<version>. |
| low | `scripts/check_seam.sh:71` | vacuous-assertion | E-11 section 2: no source file in the open half may reference a commercial crate (nemr-storage/crypto/sync/cloud). |
| low | `scripts/ci_provision_host.sh:158` | discarded-status | The unprivileged-user-namespace precondition that the E-11 offline test (export/import with no network and no credential) depends on. |
| low | `scripts/git-hooks/pre-push:8` | disabled-gate | The fmt pre-push guard — blocks a push whose tree is not rustfmt-clean, in both workspaces. |
| low | `scripts/install_server.sh:1` | not-executed | The 252-line self-hosted sync-server installer: Postgres, the auth pepper, the bundle store and the nemr-sync service. |
| low | `scripts/install_server.sh:185` | discarded-status | That the sync server's auth pepper — the secret whose loss locks out every account (E-19, F-89) — is 32 bytes of real entropy. |
| low | `scripts/install_sync_client.sh:50` | silent-skip | That all eight `nemr-*` names on PATH resolve to THIS build (the F-7 staleness gate). |
| low | `scripts/lib/cat.sh:17` | not-executed | The documented direct-execution entry points `./scripts/lib/cat.sh --show` (print the four frames) and `--demo` (animate for 5s, then clear), and with |
| low | `scripts/setup_host.sh:1` | not-executed | The 495-line developer provisioner: installs the engine, the privileged helper and its sudoers grant, the base image, the sync test DB, the sync clien |
| low | `scripts/setup_host.sh:469` | silent-skip | Steps 9c and 9d of developer provisioning: the sync-server test Postgres and the sync client (nemr login/push/pull/sessions). |
| low | `scripts/setup_sync_test_db.sh:38` | discarded-status | `--stop`: remove the Postgres test container. |
| low | `scripts/test_install.sh:56` | not-executed | Setup for the 'Rust toolchain removed for real' preflight case — a shim directory plus a loop that looks like it installs command shims. |
| low | `scripts/test_proc_lib.sh:186` | uncounted | The wait-helper library's own tests — wait_for_service, wait_for_ready, require_tcp, output_has, refuse_protected, NEMR_PROTECTED_SUBJECTS. |
| low | `tests/common/mod.rs:173` | disabled-gate | require_serial_execution() — F-71's assertion that the regression suite runs with --test-threads=1, because these tests attach loop devices, mount fil |
| low | `tests/regression.rs:4906` | uncounted | f125_git_guard_acceptance_suite_passes asserts `out.status.success() && stdout.contains("0 red") && stdout.ends_with("PASS\n")` on scripts/test_git_gu |
