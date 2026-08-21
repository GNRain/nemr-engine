# Decisions

Product-owner rulings on escalations and architectural questions. Section 1–3
decisions and Section 9 escalations are Rain's to make; this file is the record.

**Maintenance rules.** Append-only — never delete an entry, never rewrite an
existing entry's rationale. If a ruling changes, mark the old one `Superseded`
with its rationale intact and write a new entry referencing it. Claude Code may
add to **Open** but may not move anything to **Resolved**; that edit is Rain's.
Every entry states its consequences — a decision without consequences is a
preference. Update the Log table on every change.

**Status values:** Open · Resolved · Superseded
**ID prefixes:** `D-xx` product decisions · `E-xx` escalations (shared namespace with SPEC Section 9) · `F-xx` findings from the conformance ledger

---

## Resolved

### D-01 — Compute model: local compute, cloud storage

**Status:** Resolved · 2026-08-20

**Question.** Does the container engine run on the user's machine, with the
cloud used only for storage and coordination, or do we host the compute?

**Ruling.** Local compute, cloud storage. The engine runs on the user's
machine. The cloud holds bundles, the session index, and the lease service.

**Rationale.** Keeps us a sync product rather than a hosting company: software
margins, no per-user compute cost, no idle-reaping or abuse-handling burden,
and Claude Code API traffic never egresses from our infrastructure. It also
preserves the open-core split — the engine stays open source and the
portability layer is the commercial product.

**Consequences.**
- The stack (loopback ext4, rootless containerd, runc) is Linux-only. Windows
  and macOS require a bundled VM. **Top product risk**, not a footnote. See E-10.
- Every attach is a download from object storage, so egress cost scales with
  active usage. See D-05.
- The engine needs a stable consumption interface for the GUI. See E-09.

---

### D-02 — Credentials: per-device, never synced

**Status:** Resolved · 2026-08-20
**Relates to:** M9 exclusion policy, import UX, F-12

**Question.** Do credentials travel inside an exported bundle — encrypted,
stripped and re-prompted, or something else?

**Ruling.** Per-device credentials, injected at attach, never synced. The
Claude Code API key and any OAuth tokens are excluded from the bundle
unconditionally.

**Rationale.** A synced bundle is at rest in object storage and in transit
across machines we don't control. Plaintext is unacceptable; encrypted-in-bundle
just moves the problem to key distribution, which is the same problem again.
Per-device keeps the secret's blast radius at one machine.

**Consequences.**
- **A session bundle is not self-sufficient.** Import on a fresh machine
  requires a separate credential step — the first thing a new user hits, so it
  belongs in the import flow's UX rather than bolted on.
- M9's exclusion policy treats credential paths as a hard exclusion, tested,
  not a configurable default.
- Enforcement depends on every credential-bearing path being enumerated. WP C
  did this: the credential is a read-only host bind-mount, off both portable
  layers. `.claude.json` holds machine and account identity that must not travel.
- Rotation must propagate to a running container, or per-device injection is
  only true at create time. See F-12.

**Verification.** WP C confirmed the credential sits outside both portable
layers, and WP C2's implementation keeps it there deliberately — the
whole-directory `CLAUDE_CONFIG_DIR` relocation was rejected on these grounds.

---

### D-03 — Concurrency: server-side lease

**Status:** Resolved · 2026-08-20

**Question.** What prevents two machines attaching the same session and
corrupting it?

**Ruling.** A server-side lease per session: TTL, client heartbeat to renew,
and an explicit forced-takeover path in the UI ("This session is open on
MacBook Pro — take over?").

**Rationale.** Sessions are single-writer and mutable. Client-side coordination
can't survive a network partition or a crashed client; the lease has to live
where both clients can see it. TTL plus heartbeat means a dead client releases
its lease without manual intervention.

**Consequences.**
- Requires a server component beyond object storage — the lease can't live in
  the bucket. Minimum viable backend: identity, session index, lease.
- The heartbeat must outlive the GUI process. This was a deciding input to E-09.
- Forced takeover needs a defined answer for the loser's unsynced state.
  Specify before shipping: reject the stale upload, or accept it into a
  conflict bundle.

---

### D-04 — Dirty-close recovery: snapshot on quiesce

**Status:** Resolved · 2026-08-20
**Supersedes:** the fixed 5-minute sync timer proposed earlier the same day

**Question.** The laptop lid closes mid-session. What has been persisted?

**Ruling.** Snapshot on quiesce. Watch the transcript file; 2 seconds after the
last write, snapshot and upload asynchronously. Retain a 5-minute timer as a
floor for the case where writes never quiesce.

**Superseded proposal, kept deliberately.** The original instinct was a fixed
5-minute sync interval. Rejected because it uploads mid-write (torn state),
uploads when nothing has changed (wasted egress), and still loses up to five
minutes in the worst case. Agent sessions are bursty — a quiesce trigger gives
near-zero loss and fewer uploads than a fixed interval. Recorded so the
fixed-interval idea isn't re-proposed on its surface appeal.

**Consequences.**
- The snapshot must be crash-consistent; the quiesce window is what avoids a
  torn upload, so the watcher's correctness is load-bearing.
- Uploads must be resumable. Lid-close mid-upload is the common case.
- The watcher must run whenever a session is live, GUI open or not. Second
  deciding input to E-09.
- "What did I lose?" needs a user-visible answer at next attach.

---

### D-06 — Bundle format: file-level, base image by digest

**Status:** Resolved · 2026-08-20 · dependency discharged 2026-08-21
**Relates to:** M9, M11, M13

**Question.** What does an exported session bundle contain, and in what form?

**Ruling.**

1. **File-level, not image-level.** Tar from inside the mounted volume rather
   than snapshotting the `.img`.
2. **Don't ship the rootfs.** Reference the base image by digest; reconstruct
   on the destination.
3. **Chunk plaintext → compress each chunk → encrypt each chunk.** In that
   order. zstd as the codec.
4. **The manifest separates session-critical from reconstructible content**, so
   import can materialize lazily.

**Rationale.** Image-level bundles track the volume's high-water mark and never
shrink, and ext4 metadata churn destroys delta efficiency. Compress-then-chunk
kills dedup — one byte early in the stream shifts every downstream boundary —
which is why restic and borg chunk first. Referencing the base image rather
than carrying it keeps bundles in megabytes instead of gigabytes.

Exclusion policy, not codec choice, is the compression strategy: dropping
`target/` and `node_modules/` saves ~95%; zstd-vs-xz is worth ~20%.

**Consequences.**
- We own uid/gid mapping across the user namespace, plus xattrs, sparse files
  and hardlinks. Bounded work, but ours.
- Base image reproducibility is load-bearing — hence M11's drift hardening.
- Cold attach on a new machine is a full download. Mitigated by cross-project
  dedup for shared base images and by lazy materialization, both of which
  depend on point 4 being in the manifest from the start.
- **M9 must be built with the chunk boundary as a seam.** A single compressed
  stream makes M13 a format rewrite.

**Dependency — discharged 2026-08-21, and how matters.** This ruling assumed
session history lived on the portable volume. **That premise was false.** WP C1
found only the project file on the volume; the 24 KB transcript, session state
and config were all on the ephemeral rootfs, and an export of the volume would
have carried zero history. M8's surgical bind-mounting of
`/root/.claude/{projects,sessions}` is what made the ruling true — the finding
did not confirm it. D-06 is therefore contingent on relocation continuing to
hold, not on an observed property of Claude Code. Any change to M8's mount
strategy invalidates this entry until re-verified.

---

### E-09 — Engine consumption model: daemon over UDS gRPC

**Status:** Resolved · 2026-08-21
**Recorded in:** SPEC Section 9

**Question.** Long-running daemon with IPC/gRPC, or a library linked into the
GUI process?

**Ruling.** A long-running user daemon, gRPC over a Unix domain socket.

**Rationale.** Two already-resolved decisions require something running when
the GUI isn't. D-03's lease must keep heartbeating with the lid shut, or a user
is locked out of their own session from another machine until TTL expiry.
D-04's quiesce watcher must observe the transcript whenever a session is live,
which under D-01 means whenever the container is up — GUI open or closed. A
library forces both into a process users close, or bolts on a background helper
later, which is a daemon arrived at by accident with an undesigned IPC surface.

Secondary: CLI and GUI will both exist and must not disagree. Two processes
independently mutating containerd state and mount records is the divergence
class WP A spent nine commits eliminating. A daemon makes single-writer
structural rather than conventional.

**Consequences.**
- **Unix domain socket, not TCP.** No port, no localhost binding; filesystem
  permissions authenticate. If E-10's remote-engine fallback ever happens, the
  transport swaps and the service definition doesn't.
- Version handshake from day one, same pattern as the helper's protocol
  version — refuse a client/daemon mismatch cleanly.
- The daemon is a client of the `nemr-containerd` crate, not a replacement.
  The crate stays usable standalone. WP B proved this.
- The CLI talks to the daemon; it keeps no second direct path into containerd.
- Accepted costs: service lifecycle management, client/daemon version skew, and
  an IPC surface that becomes a compatibility obligation once third parties
  touch it.

---

## Open

### E-10 — Non-Linux hosts

**Status:** Open
**Escalated by:** D-01 · **Blocks:** GUI planning, the Windows/macOS product

Loopback ext4 plus rootless namespaces is Linux-only. The product vision is a
cross-platform GUI. With D-01 putting compute on the user's machine, this is a
direct contradiction rather than a deferred concern.

Options to cost out: bundled VM (the Docker Desktop route — note that's why
Docker Desktop is a ~500MB install); WSL2 on Windows with something else on
macOS; or a remote-engine fallback that partially reverses D-01.

---

### E-11 — Open-core seam

**Status:** Open · **decide now**
**Blocks:** WP D

What stays in `nemr-engine` versus the commercial portability/sync layer.

The crate boundary is no longer hypothetical: WP B extracted
`crates/nemr-containerd`, proved it standalone, and CI-verified it. This is the
cheapest moment to draw the line — WP D builds on top of it, and retrofitting a
seam through a codebase is a tax paid for years.

**Working position, not yet a ruling.** `nemr-containerd` and the engine stay
open. The sync layer, lease service, and cloud storage backends are commercial.
The **bundle format stays open and documented** — a proprietary format
undermines the open-core story and makes the engine useless without the paid
layer, which is the opposite of the intent.

---

### D-05 — Storage backend

**Status:** Open — technical direction set, commercial choice deferred
**Relates to:** M12

**Direction.** Build Cloudflare R2 first behind the M12 S3-compatible trait;
keep Backblaze B2 viable behind the same interface. No vendor-specific behavior
above the trait.

**Numbers as of 2026-08-20 — re-verify before committing commercially:**

| | Egress | At rest |
|---|---|---|
| AWS S3 | ~$0.09/GB | ~$0.023/GB-month |
| Cloudflare R2 | $0 | ~$0.015/GB-month |
| Backblaze B2 | free up to 3× stored | ~$0.006/GB-month |

The workload is egress-heavy — every attach is a download — so R2 wins on the
line item that scales with usage. B2 is cheaper at rest, but its free egress is
capped as a multiple of stored bytes, and a sync product with small bundles and
frequent pulls can exceed 3× without much effort.

**Still to decide.** Default backend at launch; whether tiers map to different
providers.

---

### D-07 — Error model: unify now or with the daemon

**Status:** Open · Rain's position below, ruling pending
**Raised by:** Claude Code, WP B

Claude Code deferred error-model unification, recommending it land with the
daemon on the grounds that gRPC status mapping can't be designed until the IPC
surface exists.

**Rain's position.** The recommendation conflates two layers. The internal
`thiserror` enum and the gRPC status mapping are independent: the mapping lives
at the daemon boundary and can be added later without touching the enum.
Deferring the whole thing means WP D writes error handling twice and then
rewrites it. **Do the enum now; defer the status codes.**

---

### F-12 — Credential bind-mount pins an inode

**Status:** Open · verified, tracked, not fixed
**Feeds:** M9 · **Interacts with:** D-02

The credential bind-mount pins an inode, so host-side rotation is invisible
inside a running container. Per-device credentials injected at attach only hold
if rotation propagates; otherwise D-02 is true at create time and drifts
thereafter.

---

### F-XX — `.claude.json` MCP-vs-identity split

**Status:** Open · **needs ruling before M9's exclusion policy is written**
**Feeds:** D-06 manifest, M9

`.claude.json` mixes two kinds of content. MCP configuration should ideally
travel with a bundle so a session resumes with its tools intact. Machine and
account identity must not travel, per D-02.

That's a manifest-level split rather than a whole-file include/exclude, and it
has to be decided before M9's exclusion policy is written. Claude Code to
supply a recommendation; assign a real F-number from the conformance ledger.

> **Implementation note (2026-08-21, Claude Code — not a status change).** This
> entry is **F-54** in the conformance ledger; the follow-on implementation gap
> — MCP configuration could not travel at all, because `.claude.json` lives on
> the rootfs rather than the volume — is **F-55**.
>
> Ruled as option (a) with the constraint that only the portable subset reach the
> volume. Implementing it turned out not to need a bind-mount at all: Claude Code
> natively reads **project-scoped** MCP configuration from `.mcp.json` at the
> project root, and the project root *is* the volume (verified — `claude mcp list`
> discovers a server declared only there). So MCP configuration travels as an
> ordinary member while `machineID`/`oauthAccount` stay on the rootfs and never
> reach the exportable layer.
>
> That satisfies the constraint **structurally rather than by filtering**, which
> was the point of the ruling: no filter can fail open on a field that was never
> there. Because no bind-mount changed, **D-06 was not invalidated** — the M8 and
> M10 acceptances were re-run regardless and both pass.
>
> Scope limit worth knowing: *project*-scoped MCP config travels; *user*-scoped
> config (stored in `.claude.json`) does not, and arguably should not travel with
> a project bundle. The `.claude.json` filtering code was removed rather than left
> in place, since a filter no export reaches is a defence that only looks like one.

---

### F-58 — the guard-test rule found seventeen defects, including a path traversal

**Status:** Open (record; no ruling required unless the scope below is disputed)
**Raised by:** Claude Code · **Relates to:** F-56, F-57, the `.claude/loop.md` rule

Applying the standing rule — *a guard test must be proven to fail when the
guarded property is violated* — across the suite produced **seventeen** confirmed
findings, not the handful expected. All are closed; each fix was verified by
disabling the guarded code, watching the test go red, and restoring it.

The most serious was a **path traversal in `extract()`**. The traversal guard
existed and was unit-tested, but nothing asserted that the extract path *used*
it: replacing `safe_join(destination_root, &member.path)` with a plain
`destination_root.join(...)` left every test green while a hostile bundle wrote
outside the destination. This is the same defect class as the original
privileged-helper mount escalation, in new code, reached by untrusted input —
a bundle may arrive from another machine or another user.

Four others guarded nothing outright: the kernel-5.8 floor test compared tuple
literals to tuple literals; the extraction-ordering test re-ran the production
sort on its own data; the schema-refusal test let `open()`'s check be deleted;
and the `.claude.json` filter tests covered a code path no real export reaches.
Four tests were deleted rather than fixed, being tautologies or assertions
against a constant production no longer consults.

**Consequences.** The rule is worth its cost and should stay standing: it found a
traversal that ordinary review, unit tests and an adversarial audit for *bugs*
had all missed, because the tests were green. It also implies a habit for new
code — a guard is not done when its test passes, only when its test has been
seen to fail. Two of the seventeen (F-56, F-57) were found before the audit and
motivated it; the audit found the rest.

---

## Log

| Date | Entry | Change |
|---|---|---|
| 2026-08-20 | D-01 | Resolved — local compute, cloud storage |
| 2026-08-20 | D-02 | Resolved — per-device credentials, never synced |
| 2026-08-20 | D-03 | Resolved — server-side lease with TTL and heartbeat |
| 2026-08-20 | D-04 | Resolved — snapshot on quiesce; supersedes fixed 5-min timer |
| 2026-08-20 | D-05 | Opened — R2 first behind the M12 trait; default deferred |
| 2026-08-20 | D-06 | Resolved — file-level bundle, base image by digest |
| 2026-08-20 | — | Numbering reconciled: one global `E-` namespace shared with SPEC Section 9; provisional E-01/02/03 renumbered to E-09/10/11 |
| 2026-08-21 | E-09 | Resolved — daemon, gRPC over Unix domain socket |
| 2026-08-21 | D-06 | Dependency discharged — premise was false; M8 relocation made the ruling true |
| 2026-08-21 | D-02 | Verification from WP C recorded; F-12 noted as a rotation gap |
| 2026-08-21 | E-11 | Working position added; crate boundary now real and CI-verified |
| 2026-08-21 | D-07 | Opened — error model; Rain's position recorded, ruling pending |
| 2026-08-21 | F-12 | Opened — credential bind-mount inode pin |
| 2026-08-21 | F-XX | Opened — `.claude.json` MCP-vs-identity split |
| 2026-08-21 | F-XX | Implementation note appended — ledger number is F-54; resolved via project-scoped `.mcp.json`, structurally, no bind-mount change, D-06 intact (Claude Code) |
| 2026-08-21 | F-58 | Opened — guard-test rule produced 17 findings incl. an `extract()` path traversal; all closed (Claude Code) |
