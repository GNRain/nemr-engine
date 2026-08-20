# Decisions

Product-owner rulings on escalations and architectural questions. Section 1–3
decisions and Section 9 escalations are Rain's to make; this file is the record.

Claude Code appends new escalations to **Open** with options and a
recommendation, and does not move them to **Resolved** — that edit is Rain's.

**Status values:** Open · Resolved · Superseded
**ID prefixes:** `E-xx` escalations · `D-xx` decisions raised outside the escalation path

> **Escalation numbering (reconciled 2026-08-21).** `E-` is one global namespace
> shared with SPEC.md Section 9, which is the canonical ledger. SPEC owns
> `E-01`…`E-08` (the original spec escalations, plus `E-07` the attach-pty drift
> and `E-08` the helper-hardening visibility item). The three product gates
> recorded in **Open** below as `E-01`/`E-02`/`E-03` are SPEC's **`E-09`**
> (engine consumption — resolved), **`E-10`** (non-Linux hosts) and **`E-11`**
> (open-core seam). They are pending renumber to those IDs in this file — flagged
> for the Product Owner because it restructures existing entries, which the
> maintenance rules reserve. `D-0x` is a separate series for decisions raised
> outside the escalation path and does not collide.

---

## Resolved

### D-01 — Compute model: local compute, cloud storage

**Status:** Resolved · 2026-08-20
**Supersedes/blocks:** GUI planning, non-Linux host support

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
  and macOS require a bundled VM. **This is now the top product risk**, not a
  footnote, and it blocks GUI planning. See `E-02`.
- Every attach is a download from object storage, so egress cost scales with
  active usage. See `D-05`.
- The engine needs a stable consumption interface for the GUI. See `E-01`.

---

### D-02 — Credentials: per-device, never synced

**Status:** Resolved · 2026-08-20
**Relates to:** M9 exclusion policy, import UX

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
  requires a separate credential step. This is the first thing a new user
  hits, so design it into the import flow's UX now rather than bolting it on.
- M9's exclusion policy must treat credential paths as a hard exclusion, tested,
  not a configurable default.
- WP C's state-locality findings must enumerate every path that holds a
  credential or token, or this ruling can't be enforced.

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
  the bucket. This is the minimum viable backend alongside identity and the
  session index.
- Forced takeover needs a defined answer for the loser's unsynced state.
  Specify it before shipping: reject the stale upload, or accept it into a
  conflict bundle.

---

### D-04 — Dirty-close recovery: snapshot on quiesce

**Status:** Resolved · 2026-08-20
**Supersedes:** the earlier proposal of a fixed 5-minute sync timer

**Question.** The laptop lid closes mid-session. What has been persisted?

**Ruling.** Snapshot on quiesce. Watch the transcript file; 2 seconds after the
last write, snapshot and upload asynchronously. Retain a 5-minute timer as a
floor for the case where writes never quiesce.

**Rationale.** A fixed timer uploads mid-write (torn state), uploads when
nothing has changed (wasted egress), and still loses up to five minutes in the
worst case. Agent sessions are bursty — a quiesce trigger gives near-zero loss
and fewer uploads than a fixed interval.

**Consequences.**
- The snapshot must be crash-consistent. If the transcript is mid-append when
  the snapshot fires, it's a torn upload — the quiesce window is what avoids
  this, so the watcher's correctness is load-bearing.
- Uploads must be resumable; a lid-close mid-upload is the common case, not the
  edge case.
- "What did I lose?" needs a user-visible answer at next attach.

---

### D-06 — Bundle format: file-level, base image by digest

**Status:** Resolved · 2026-08-20
**Relates to:** M9, M11, M13

**Question.** What does an exported session bundle actually contain, and in
what form?

**Ruling.** Four parts:

1. **File-level, not image-level.** Tar from inside the mounted volume rather
   than snapshotting the `.img`.
2. **Don't ship the rootfs.** Reference the base image by digest; reconstruct
   on the destination.
3. **Chunk plaintext → compress each chunk → encrypt each chunk.** In that
   order. zstd as the codec.
4. **Manifest separates session-critical from reconstructible content**, so
   import can materialize lazily.

**Rationale.** Image-level bundles track the volume's high-water mark and never
shrink, and ext4 metadata churn destroys delta efficiency. Compress-then-chunk
kills dedup — one byte early in the stream shifts every downstream boundary —
which is why restic and borg chunk first. Referencing the base image rather
than carrying it is what keeps bundles in megabytes instead of gigabytes.

Exclusion policy, not codec choice, is the compression strategy: dropping
`target/` and `node_modules/` saves ~95%, while zstd-vs-xz is worth ~20%.

**Consequences.**
- We own uid/gid mapping across the user namespace, plus xattrs, sparse files,
  and hardlinks. Bounded work, but ours.
- Base image reproducibility becomes load-bearing, which is why M11 lists base
  image drift as a hardening concern.
- Cold attach on a new machine is a full download. Mitigated by cross-project
  dedup for shared base images and by lazy materialization — both of which
  depend on point 4 being in the manifest from the start.
- **M9 must be built with the chunk boundary as a seam.** A single compressed
  stream makes M13 a format rewrite.

**Dependency.** All of this assumes session history lives on the portable
volume. If WP C finds it in the rootfs snapshot, this ruling describes a
container with nothing in it, and M8's relocation work is what makes it
coherent. Re-confirm after `docs/state-locality.md` lands.

**Dependency resolved (2026-08-21, per WP-C — Claude Code).** The premise was
**false**. `docs/state-locality.md` measured session history on the ephemeral
rootfs, not the portable volume; an export of the volume would have carried zero
history. This ruling was therefore *not* confirmed by an observed property of
Claude Code — it was made **true** by M8's relocation, which bind-mounts the
history subtrees (`projects/`, `sessions/`) onto the volume. **D-06 is contingent
on relocation continuing to hold**, not on where Claude Code writes by default.
If relocation regresses, or Claude Code moves its history path, this ruling
silently describes the wrong bytes again; the M8 regression test
(`m8_session_state_lives_on_the_volume_and_vanishes_when_unmounted`) is the guard.

---

## Open

### E-01 — Engine consumption model

**Status:** Open
**Blocks:** GUI work, WP-B crate extraction

Long-running daemon with IPC/gRPC, or a library linked into the GUI process?
Touches Sections 1–3.

The WP-B containerd wrapper crate is currently being designed to keep both
options open, which costs complexity until this is ruled on. Don't let that
persist longer than WP B.

---

### E-02 — Non-Linux hosts

**Status:** Open
**Escalated by:** `D-01`
**Blocks:** GUI planning, the Windows/macOS product

Loopback ext4 plus rootless namespaces is Linux-only. The product vision is a
cross-platform GUI. Now that `D-01` puts compute on the user's machine, this is
a direct contradiction rather than a deferred concern.

Options to cost out: bundled VM (the Docker Desktop route — note that's why
Docker Desktop is a ~500MB install), WSL2 on Windows with something else on
macOS, or a remote-engine fallback that partially reverses `D-01`.

---

### D-05 — Storage backend

**Status:** Open (technical direction set, commercial choice deferred)
**Relates to:** M12

**Direction.** Build Cloudflare R2 first behind the M12 S3-compatible trait,
keep Backblaze B2 viable behind the same interface. No vendor-specific behavior
above the trait.

**Numbers as of this writing — re-verify before committing commercially:**

| | Egress | At rest |
|---|---|---|
| AWS S3 | ~$0.09/GB | ~$0.023/GB-month |
| Cloudflare R2 | $0 | ~$0.015/GB-month |
| Backblaze B2 | free up to 3× stored | ~$0.006/GB-month |

Our workload is egress-heavy — every attach is a download. R2 wins on the line
item that scales with usage. B2 is cheaper at rest, but its free egress is
capped as a multiple of stored bytes, and a sync product with small bundles and
frequent pulls can exceed 3× without much effort.

**Still to decide.** Which backend is the default at launch, and whether tiers
are backed by different providers.

---

### E-03 — Open-core seam

**Status:** Open
**Raise early, not late**

What stays in `nemr-engine` versus the commercial portability/sync layer.
Retrofitting a seam through a codebase is a tax paid for years, so this wants
deciding while WP B is still moving module boundaries.

---

### F-12 — credential rotation must propagate into a running container

**Status:** Open · feeds M9
**Relates to:** `D-02`, AUTH-02
**Raised by:** Claude Code (WP-C finding)

The host credential is delivered as a read-only **bind mount of a single file**
at `/root/.claude/.credentials.json`. WP-C1 confirmed it is the only secret and
is on neither portable layer, so `D-02` holds — but a file bind-mount pins an
**inode**. If Claude Code on the host rotates the credential by atomic replace
(write-new-then-rename, the safe and common pattern), the container keeps reading
the old inode. Verified as a finding, not yet fixed.

**Why it is a decision, not only a fix.** `D-02` says credentials are per-device,
injected at attach. That holds end to end only if rotation *propagates*; a stale
token in a long-lived container fails silently against the API. The fix options
trade off — bind-mount the parent directory instead of the file, re-resolve on
each attach, or watch-and-remount — and they touch the attach path WP D depends
on.

**Consequences.** Forecloses nothing yet; obliges M9's credential handling to
assume rotation is visible, which it currently is not. If left unfixed, a
long-running session on a rotated host silently loses API access.

**Recommendation (Claude Code):** bind-mount the credential's parent directory
rather than the file, so a rotated inode is visible without a remount, and
re-verify at attach. Decision yours before M9's exclusion policy hardens around
the current single-file shape.

---

### D-07 — `.claude.json`: split portable config from machine identity

**Status:** Open · feeds M9 and `D-06`'s exclusion policy
**Raised by:** Claude Code (WP-C finding)

`/root/.claude.json` mixes two kinds of content (measured in WP-C1): **portable
configuration** — MCP server definitions and project-trust entries, which a user
would want to travel with a session — and **machine/account identity**
(`machineID`, `userID`, `oauthAccount` with email, organization and billing),
which must *not* travel: it identifies the source device and account. It is
currently kept wholly on the rootfs, off the volume, so nothing travels — safe,
but MCP configuration is lost on import.

**Consequences.** `D-06`'s manifest must decide this before M9's exclusion policy
is written, or M9 will either drop MCP config (a usability regression on import)
or carry the identity block (a `D-02`-adjacent leak). It forecloses a file-level
include/exclude of `.claude.json`; the split has to be finer than the file.

**Recommendation (Claude Code):** treat `.claude.json` as a **field-level** split,
not file-level — the bundle carries an allowlist of portable keys (`mcpServers`,
project trust) and never the identity block. I can prototype the field filter
behind `D-06`'s pluggable exclusion policy when M9 starts. Decision yours.

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
| 2026-08-20 | E-01 | Opened — engine consumption model |
| 2026-08-20 | E-02 | Opened — non-Linux hosts, escalated by D-01 |
| 2026-08-20 | E-03 | Opened — open-core seam |
| 2026-08-21 | D-06 | Dependency resolved — premise was false; M8 relocation made the ruling true (Claude Code) |
| 2026-08-21 | F-12 | Opened — credential rotation must propagate; feeds M9 (Claude Code) |
| 2026-08-21 | D-07 | Opened — .claude.json field-level portable/identity split; feeds M9 (Claude Code) |
| 2026-08-21 | — | Numbering note reconciled: E- shared with SPEC §9; E-01/02/03 here = SPEC E-09/10/11, renumber pending PO |
