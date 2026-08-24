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

### D-08 — Where the base image lives, and what happens when it does not

**Status:** **Resolved** (2026-08-22) — GHCR, by digest, with a local fallback.
**Raised by:** Claude Code · **Ruled by:** Rain · **Relates to:** D-06, E-11, M11

`nemr` pins its base image as `docker.io/nemr/base:0.1.0`. Three facts about
that string, established rather than assumed:

**Is it published anywhere?** **No — measured, not assumed.** An anonymous
manifest request to `registry-1.docker.io` for `nemr/base` returns **HTTP 401**
for both `0.1.0` and `latest`, while the same probe returns **HTTP 200** for
`library/alpine` and `library/busybox` — the control that proves the probe finds
a repository that does exist. Docker Hub answers 401 rather than 404 for
absent-or-private repositories, so the precise finding is *not anonymously
pullable*; whether it is missing or private cannot be distinguished from
outside. Either way a pull would fail for a new user.

The name was written into `config.rs` as a placeholder and never registered.
Every working install today has the image because it was built locally and
imported into containerd by hand, per README §"Base image (Milestone 2)". The
reference is therefore a *label for a local artifact*, not a location — and
nothing in the code or the docs says so.

**What happens on registry outage?** Today, nothing — because nothing pulls.
That is not a resilience property, it is the absence of a code path. The moment
a pull is implemented against this name, an outage (or a Docker Hub rate limit,
which is far likelier: anonymous pulls are throttled per-IP) becomes a hard
failure at `nemr start` on any machine that has not already cached the image.
Note the shape: the failure would arrive on a machine that has run fine for
months, at the moment its cache is evicted.

**Does E-11 constrain where base images live?** No — and this is worth stating
plainly because it is the one part that is genuinely unconstrained. E-11 puts
the engine, the containerd wrapper, the volume layer, the helper and the bundle
*format* on the open side, and the sync layer, lease service, cloud storage
backends and identity on the commercial side. A base image is none of those. It
is an input the open engine consumes, like `runc`. The E-11 test — *can someone
use the open half productively without ever paying?* — is satisfied as long as
the image is obtainable without an account. It is **not** satisfied if the image
moves behind a registry that requires a paid identity, which is the one option
below that should be treated as foreclosed.

There is a fourth fact that matters more than the hosting question: `docker.io`
is a **Docker-branded registry**, and the project's hard constraint is *no Docker
at any layer, including transitively*. Pulling from Docker Hub does not link
Docker code — containerd speaks the OCI distribution protocol and the registry
is just an HTTPS host — so this is a naming and dependency-posture question, not
a literal breach. But shipping a product that forbids Docker while its default
image reference begins `docker.io/` is the kind of detail that will be read as
one, and I would rather raise it than let it be discovered.

**Options.**

| # | Option | Cost | What it buys | What it costs |
|---|---|---|---|---|
| a | **Keep the name, publish the image** to Docker Hub under a real `nemr` org | low | The reference becomes true; `nemr start` can pull | Registry dependency on `docker.io` at first run; rate limits; the branding problem above |
| b | **Rename to a vendor-neutral registry** (GHCR, `ghcr.io/<org>/nemr-base`) and publish | low | Same as (a), without `docker.io` in the string; GHCR has no anonymous pull limit for public images | Still a single hosted dependency and a single point of outage |
| c | **Keep it local-only and say so**: rename to a non-registry reference (e.g. `nemr.local/base:0.1.0`), keep the build-and-import step, add a check that fails with a build instruction rather than attempting a pull | low | Honest today; no network dependency at all; no registry to go down | Every user must build the image; slower first run; no upgrade path without one |
| d | **Content-addressed with a mirror list**: pin by digest (D-06 already does this for bundles), fetch from any of N hosts, fall back to local build | high | Outage-tolerant, verifiable, vendor-neutral | Real work — mirror infrastructure, digest pinning for images, fallback logic — for a problem nobody has hit yet |

**Recommendation: (c) now, (b) when there is something to ship.**

The reasoning is that (c) is the only option that makes the code *stop lying*
today, at near-zero cost. The current string asserts a registry location that
does not exist; renaming it to something obviously local converts a latent
404-on-first-pull into an accurate description of what the system actually does.
That is a strictly better position to be in while pre-release, and it forecloses
nothing. (b) is where this should land once there is a published artifact worth
pulling, and it avoids both the `docker.io` branding problem and Docker Hub's
anonymous rate limits. (d) is correct engineering for a mature product and
premature now — it buys resilience against an outage of infrastructure that has
not been chosen yet.

**Consequences of each, since the ruling is yours:**

- **(a)** commits the project to `docker.io` in its most visible default string,
  and to Docker Hub's rate limits on every uncached first run. Reversing it
  later means changing a reference users have already pinned.
- **(b)** commits to GHCR as the distribution point and to maintaining a
  published image (tagging, retention, a signing story). Cheapest correct
  long-term answer; still one host that can go down.
- **(c)** commits every user to a local build step, which is a real onboarding
  cost and will be the first thing a new user complains about. It also means
  there is no mechanism to ship a base-image security fix — users must rebuild,
  and nothing tells them to.
- **(d)** commits to running mirror infrastructure, which is an ongoing
  operational cost and a commercial-side obligation E-11 has not accounted for.

**Not decided here.** I have not changed `BASE_IMAGE`; the string still reads
`docker.io/nemr/base:0.1.0` and the README still says build-and-import.

---

**RULING (2026-08-22, Rain): GHCR, by digest, with a local fallback.**
Recommendation (c)-then-(b) above is **superseded** — the ruling goes further
than the options as framed, and the reason is worth recording because it is not
a registry-choice argument.

Publish to **`ghcr.io/gnrain/nemr-base`** and reference **by digest**, not tag.
The org exists, CI already builds the image with BuildKit, and packages are free
for public repos: no new vendor, no Docker Hub rate limits, no account outside
our control.

The registry choice is the smaller half. **The real issue is that the product's
core promise currently depends on a third party being reachable.** "Your session
comes back on another machine" fails if the registry is down. M11 hardens
against base-image *drift*; registry *outage* is a different failure and is not
covered.

Three parts, all required:

1. **Publish to GHCR and reference by digest**, not tag. The publish is wired
   into CI, and a check fails the build if the digest in `BASE_IMAGE` and the
   published artifact diverge — the same argument as the hash gates.
2. **`nemr import` resolves a locally cached image matching the digest before it
   reaches the network.** If the bytes are already on the host, importing must
   not require the registry at all.
3. **`nemr export --with-base-image`** as an opt-in that embeds the image for
   genuinely offline transport. Default stays reference-by-digest, preserving
   D-06's size argument; the flag exists for when self-sufficiency matters more
   than size.

**Consequence (recorded on Rain's instruction): an open format that only works
when a specific company's registry answers is not fully open.** Part 3 is what
keeps E-11 honest — it is not a convenience feature, and it may not be dropped
as one.

**Also required:** when a digest cannot be resolved and no local copy exists,
the error must name the digest, say where resolution was attempted, and
distinguish *registry unreachable* from *digest not found there*. Those need
different fixes from the user, and an error that conflates them sends people to
the wrong one.

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

## Deferred (decided, with direction — not gaps)

A deferred entry is a **decision to sequence work later**, with the direction
settled so nothing downstream is designed against the wrong assumption. It is
not an Open item: Open means unresolved.

### E-10 — Non-Linux hosts

**Status:** **Deferred with direction** (2026-08-22). Was Open since the first
brief.
**Ruled by:** Rain · **Escalated by:** D-01 · **Relates to:** E-11

Ship Linux first. Validate that the sync product is what people actually want.
Then Windows, then macOS.

**Direction — binding on downstream design:**

- **Windows: WSL2.** A real Linux kernel, namespaces and loop devices, so the
  stack runs close to unmodified. Most modern Windows machines already have it.
- **macOS: a bundled VM** (Lima or similar). There is no WSL2 equivalent, which
  is why Docker Desktop is a ~500MB install. Genuinely expensive, and second in
  line for that reason.
- **Remote-engine fallback: rejected.** It reverses D-01 and turns the project
  into a hosting company. Do not design toward it.

**Why deferred rather than started:** there is a working engine today and zero
users. Six months of VM packaging before validating demand is the expensive
version of the mistake this project keeps catching — building on an unverified
premise. The same discipline that ran the state-locality experiment before the
bundle format was designed.

**Consequence:** the original E-10 framing called the Linux-only stack a *direct
contradiction rather than a deferred concern*. That framing is superseded, not
deleted: the contradiction is real and is accepted for now as a sequencing
choice with a known resolution, rather than an unresolved gap. Anything that
assumes cross-platform support today is assuming wrongly.

---

## Open

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

**Status:** **Scheduled** (2026-08-22, Rain) — promoted from tracked after
hitting it in production during the cross-machine validation.
**Feeds:** M9 · **Interacts with:** D-02

The credential bind-mount pins an inode, so host-side rotation is invisible
inside a running container. Per-device credentials injected at attach only hold
if rotation propagates; otherwise D-02 is true at create time and drifts
thereafter.

**Confirmed in the wild, and worse than tracked (2026-08-22).** On the second
host the credential had expired. Re-authenticating on the host did not help:
the running container kept the old inode and continued to fail. Recovery
required `nemr delete` and recreating the project — losing the container, not
just the session.

What the user experiences: a working credential on the host, a broken one in
the container, and **nothing that connects the two**. The engine reports no
error because from its point of view nothing is wrong; Claude Code reports an
authentication failure that the obvious remedy does not fix. That is the
VOL-05 shape applied to credentials — the system is confidently serving stale
state — and it sits directly on D-02's enforcement path, which is the whole
basis of the per-device credential model.

**Why "recreate the project" is not an acceptable remedy:** it destroys the
container to refresh a file, and on a machine where the session is the thing of
value that is a data-loss-shaped workaround for a mount bug.

**Not fixed in this pass.** Recorded as scheduled with the live evidence
attached; the fix has options (mount the directory rather than the file, or
re-resolve at attach) whose trade-offs against D-02 need stating before one is
chosen.

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

### E-15 — One agent per project; how many agents a project assumes

**Status:** RESOLVED in code (2026-08-23) — **a project does not assume Claude
Code, but it does assume one agent at a time.**
**Feeds:** WP-H · **Relates to:** D-02, the cross-agent-migration epic below

The ledger's open question was whether the design bakes in exactly one agent
(Claude Code). Answer, now implemented: the agent is a recorded property
(`nemr.agent` label, defaulting to Claude Code for anything predating the
field), chosen at `create`, reported by `status`, switchable by `nemr
switch-agent`, and carried in the bundle manifest so a restore launches the
right CLI.

**Why one at a time, not many side by side.** Each agent's CLI stores its
conversation in its own undocumented, independently-versioned format. Two agents
writing into one project would give it two disconnected pasts with no way to
tell which is authoritative — and no agent can read another's transcript
natively. So the honest model is exactly one active agent per project, and
`switch-agent` is blunt that switching does not migrate the conversation: the
old history stays on the volume but the new agent will not see it.

**The manifest records the PRODUCING agent, not merely the project's agent** —
deliberately, to leave room for the migration epic below without a schema
change. Established empirically that the field is v1-compatible in both
directions (an old reader ignores it; its absence reads as Claude Code), so no
bump was needed.

**Auth (2d), RESOLVED (2026-08-24, Product Owner):** Codex authenticates exactly
as Claude Code does — a per-device credential the user obtains on the host,
injected read-only at attach, never in a bundle. AUTH-01–03 and D-02 are
agent-agnostic (recorded in SPEC as AUTH-agent). There is deliberately no second
credential model. **But the design being settled is not the evidence:** Codex's
credential paths are unenumerated, so D-02 is *asserted* for Codex, not
*enforced* — the enumeration WP-C did for Claude Code is what makes it
enforceable, and it has not been done for Codex.

**Not finished this pass, and not faked (F-84):** where Codex keeps its session
state (the WP-C-equivalent empirical capture) and the M8/M10 portability
acceptance both need a real Codex session with API round-trips, which needs Codex
authenticated on the host. That is unavailable here — a regional purchase barrier,
not a scheduling choice. Running the capture *without* round-trips was considered
and rejected: Codex writes session state only when it talks to the model, so we
would observe an empty directory a real session would have filled — the WP-C
failure mode inverted, exactly how a "verified" feature ships having never held a
conversation. Codex is therefore marked implemented-but-unverified everywhere it
is read: the CLI warns at selection, and F-84 tracks the closure.

**Gemini as the second VERIFIED agent, later:** Gemini CLI has a free tier
reachable without a purchase, so it — not Codex — is the right target for a
genuinely verified second agent when multi-agent work resumes. The scaffolding is
agent-agnostic; only the image entry, the state-locality capture and the
credential paths are agent-specific. Not now; recorded so it is not rediscovered.

---

### Epic (post-daemon) — Cross-agent session migration

**Status:** Recorded, not scheduled. Do not design for it yet.

The motivating case: someone cancels one agent's subscription, moves to another,
and loses access to their sessions — the transcripts are still on disk
(cancelling revokes the service, not the local files) but cannot be resumed
anywhere.

What the research establishes: **lossy handoff is feasible; lossless native
resume is not.** A different model can read another's transcript as context and
continue the work, but cannot faithfully replay the original tool calls or
resume as the same conversation object. The proven pattern (the open-source
`authsec-bridge` does this across Claude Code, Codex and Gemini) is parse →
neutral intermediate representation → write in the target's native format,
demoting tool calls to prose summaries because one-to-one mapping breaks.

Nemr is well placed: it already bundles the working tree — the actual state — so
only the conversation needs transplanting, which is the tractable half. Shape
when we reach it: a neutral transcript IR in the bundle, a lossy `HANDOFF.md`
mode first, then per-target writers. Main ongoing cost and risk: every agent's
on-disk format is undocumented and changes without notice — which is also the
standing risk for the Codex session-state work above. `nemr switch-agent` is the
command that will host it; only what it can promise will change.

---

### E-13 — Authenticating on a second machine revoked the first one's credential

**Status:** Open — **escalation, raised for understanding before design.**
**Raised by:** Rain (cross-machine validation) · **Contradicts:** D-02
**Blocks:** nothing yet; **invalidates** an assumption D-02 rests on

**What was observed, precisely.** Authenticating Claude Code on the second
machine invalidated the session on the first. Same account. The first machine's
subsequent API call failed with:

```
Failed to authenticate. API Error: 401 OAuth access token has been revoked.
```

Not expiry — *revocation*. The host credential file was still present and
well-formed; the token it held had been invalidated server-side.

**Why this matters more than a login annoyance.** D-02 rules that credentials
are per-device, injected at attach, never synced — and the reason that ruling
is safe is the assumption that **each device can hold its own credential
simultaneously**. If authenticating on device B revokes device A, that
assumption is false, and the product's core promise reads:

> Your session moves between machines — and attaching on your laptop signs you
> out of your desktop.

For a product whose entire premise is machine mobility, that is a product-level
problem, not an operational one. D-02 did not anticipate it because a
single-machine test cannot surface it.

**What is NOT established, and I am not going to guess at it.** The observation
is one event on one account. All of these remain open:

- Is revocation **account-wide**, or scoped to a session/device slot?
- Is there a **concurrent session limit** (one? a small N?) rather than an
  outright replace-on-login?
- Is it **plan-dependent** — would a different subscription tier behave
  differently?
- Is it a property of the **OAuth flow** Claude Code uses, versus API-key auth,
  which may not have the same behaviour at all?

Answering any of these from the outside would be inventing facts about someone
else's auth service, and the error string is the only direct evidence we have.

**What would settle it**, cheaply and without guessing: authenticate on machine
A, confirm a call succeeds; authenticate on B; re-test A. That is the experiment
already run, once. Repeating it deliberately — and separately trying API-key
authentication rather than the OAuth flow — distinguishes "replace-on-login" from
"limit of N" and distinguishes flow-specific from account-wide.

**Options, none of them ruled:**

| # | Option | What it buys | What it costs |
|---|---|---|---|
| a | **Accept and document** — one active machine at a time, re-authenticate on arrival | Nothing to build. Honest. | Directly contradicts the product's premise; re-auth on every move is precisely the friction the product exists to remove |
| b | **Long-lived API key instead of the OAuth session** for engine-run containers | Likely sidesteps session revocation entirely | A long-lived key is a much worse secret to hold (no session expiry), and D-02's "never synced" becomes more load-bearing, not less |
| c | **Treat the credential as part of what moves** | Restores continuity | **Reverses D-02.** The credential would travel, which is the one thing three packages of structural enforcement exist to prevent. Recorded for completeness; recommending it would be arguing against the project's own posture |
| d | **Establish the actual behaviour first**, then decide | Costs one experiment | Delays a decision that is not yet blocking anything |

**Recommendation: (d), then most likely (a) with a documented limitation until
the behaviour is understood.** (b) is worth investigating only after the
mechanism is known — it may not help, and it trades a session secret for a
permanent one. (c) should stay on the table only as the thing we are choosing
*not* to do.

**Consequence of leaving it open:** F-12 (the credential bind-mount pinning an
inode) is now scheduled, and its fix should not be designed until this is
understood. Fixing rotation propagation while the underlying model is "only one
machine can be authenticated" would be solving the wrong problem carefully.

---

### D-11 — No user-facing path moves a bundle through storage

**Status:** Open — **scope statement, not a build request.**
**Raised by:** Rain (cross-machine validation) · **Relates to:** E-11, D-05, M12

There is no `nemr push` and no `nemr pull`, which is **correct** under E-11: the
sync layer is the commercial half, and putting it in the open CLI would breach
the seam the whole architecture is arranged around.

But the gap is currently total. `bucket_roundtrip` has no "fetch this key to a
path" mode either, so even internal tooling cannot complete a transfer. The
cross-machine validation was done with `rclone` — a third-party tool doing the
job the product is supposed to do.

**The distinction worth writing down:** M12 proved *the storage backend works*.
It did not prove *a user can move a session*, and those are different claims.
Everything between them — a command, credentials for it, key naming, progress,
resumption, failure recovery — is the sync layer, and it is commercial-side
scope. Stating it here means the roadmap says so rather than someone inferring
from a green M12 that the transport story is done.

**Consequence:** the honest description today is "the engine can export and
import a bundle; moving that bundle between machines is your problem." That is
a defensible Phase 1 position and an indefensible product one.

---

### D-12 — Import requires a project that does not exist yet, and a quota to invent

**Status:** Open — **scope statement; the shape is obvious, the ruling is not.**
**Raised by:** Rain (cross-machine validation) · **Relates to:** M10, D-06

`nemr import <name> <bundle>` requires the project to already exist, so
restoring onto a fresh host is three commands — `create` (with a size the user
has to invent), `import`, `start`.

The size is the part that is actually wrong rather than merely verbose: the
bundle records the source project's quota, so the user is being asked to supply
a number the file already contains. Guess low and the import refuses; guess high
and the volume is oversized forever.

The obvious shapes are `nemr import --create`, or importing into a non-existent
project by default. Which one is a product decision, not an engineering one —
hence recorded rather than built.

Worth noting what *is* working: the errors guide a user around this correctly at
every step. They are good errors. They are also guiding people around a gap,
and a well-signposted detour is still a detour.

---

### D-10 — Registry pull: out of scope now, a hard dependency the moment there is a GUI

**Status:** Open — **tracked dependency, deliberately not built.**
**Raised by:** Claude Code · **Ruled out of D-08 scope by:** Rain
**Relates to:** D-08, E-10, E-11

D-08 part 1 was scoped down: `nemr` does not fetch images. The base image is
resolved locally or the user is told the exact command to fetch it. That is
correct today and the error says so plainly rather than implying a capability
that does not exist.

**Why it is recorded separately rather than left implicit in D-08.** The
scope-down rests on one assumption: that the user is at a terminal and can run
`ctr images pull`. A GUI promising one-click attach cannot tell anyone to run
`ctr`. So registry pull is not a nice-to-have that might arrive — it is a
prerequisite of the GUI, and it should be visible as a tracked dependency now
rather than discovered during GUI work when it blocks a release.

Note the interaction with E-10: the GUI is also what makes non-Linux hosts
urgent. Both deferred items come due at the same moment, and neither is small.

**Likely approach.** containerd's **Transfer service**, exposed by
`containerd-client` 0.9 (`TransferClient`, `containerd.services.transfer.v1`) and
already vendored in the dependency we have. It moves an OCI registry source into
an image-store destination server-side, which means containerd handles the
manifest walk, layer download and content ingest — no HTTP client, no manifest
parsing and no blob handling in our code. It also composes with F-63's leases:
a transfer is exactly the kind of multi-step resource creation that needs one.

**Cost estimate — my own, and it is an estimate.** Roughly **3–5 days** of
focused work:

| Piece | Notes |
|---|---|
| Transfer plumbing | The smallest part. Wire `OCIRegistry` source → `ImageStore` destination, stream progress. ~½ day |
| Registry auth | Anonymous pull for a public GHCR image is easy; token flow for private ones is not, and the credential then falls under D-02, which forbids syncing it. Design work, not just code |
| Lease correctness | A transfer creates content and image records; the F-63 class applies directly |
| Failure taxonomy | The distinction dropped from D-08's error — unreachable vs absent vs unauthorised — becomes real and must be mapped into D-07 |
| Verification | Needs a registry to test against. A local registry in CI, or GHCR with a scoped token, which is a credential this project deliberately keeps out of the harness (see D-02 and the R2 precedent) |

The last row is the one that usually surprises: the code is days, the *evidence*
that it works is where the time actually goes, exactly as it did for M12.

**COST NOW MEASURED, NOT ESTIMATED (2026-08-22, Rain).** The ruling scoped pull
out because it was expensive work with no users. Provisioning a second host
showed the alternative's price, and it is higher than estimated:

- every new host installs a **build toolchain** — BuildKit fetched from a GitHub
  release, a second rootless systemd unit, a daemon to run — to produce an image
  that should be a download;
- the build is **not reproducible** (F-74), so two hosts following the same
  instructions get different images and M11 correctly refuses to import between
  them. That is not a bug in M11; it is the drift check working, on a drift that
  only exists because the image is built rather than distributed;
- the failure lands on a new user at the worst moment — after a two-hour setup,
  on their first import.

Publishing to GHCR turns roughly two hundred lines of build orchestration into
one `ctr images pull`, and makes F-74 moot rather than partly mitigated: an
image distributed by digest is byte-identical everywhere by construction.

**The ruling is not reversed** — recorded so the decision is re-taken against
measured cost rather than the original estimate.

**RE-ESTIMATE AFTER PUBLISHING (2026-08-23).** Asked to revisit the number now
that the image is actually published to GHCR. **It has moved, in both
directions, and the net is roughly unchanged at 3–5 days.**

What got cheaper:

- The instruction in the unresolved-base-image error becomes a one-liner that
  *works* — `ctr images pull ghcr.io/gnrain/nemr-base:0.1.0` — rather than a
  pointer to a build toolchain. That removes most of the *pain* pull was going
  to relieve, which lowers its priority even though it does not lower its cost.
- Anonymous pull of a public GHCR package needs no credential, so the
  **registry-auth row largely disappears** for the default case. That was the
  row carrying the D-02 interaction, and it was the one I flagged as design
  rather than code. Private packages would bring it back; nothing needs one.

What got more expensive, or newly appeared:

- **Digest verification becomes mandatory, not optional.** Now that a published
  digest exists and is recorded, a pull that does not verify what it fetched
  against it would be strictly worse than telling the user to run `ctr` — it
  would silently accept a substituted image. That is a new requirement pull
  did not previously carry.
- **The local-first path (part 2) becomes load-bearing rather than an
  optimisation.** Once pulling is possible, E-11's offline guarantee depends on
  resolution genuinely preferring the local copy — a pull attempted before the
  local check would break `nemr import` on a machine with no network. That is
  already implemented and tested, but it moves from "nice" to "the thing that
  must not regress", and pull work has to be built around it.
- **Verification still dominates.** The transfer plumbing is still about half a
  day; proving it works still needs a registry to test against, and now also
  needs the offline path proven *not* to reach for one.

**Net: no change to the estimate, and a clear drop in urgency.** The argument
for building it was "every new host installs a build toolchain to produce an
image that should be a download". That argument is now retired by publishing
alone — the download exists, it is one documented command, and the error names
it. What remains is the GUI case from the entry above, which is unchanged: a
one-click attach cannot tell anyone to run `ctr`.

**Recommendation: leave the ruling as it stands.** The cost did not fall enough
to change the decision, and the reason to hurry did.

**Consequence of leaving it open:** the honest description of the product today
is "brings your own base image". Any roadmap or GUI mock that shows attaching on
a fresh machine with no terminal step is describing something that does not
exist yet.

---

### D-09 — B2 is implemented but has never touched a live bucket

**Status:** Open (tracking item; no ruling required, but the gap should not
close silently)
**Raised by:** Claude Code · **Relates to:** D-05, F-61

M12's acceptance ran against a real R2 bucket and passed. The same `S3Store`
code path serves B2 — `Provider` supplies configuration only and is never
branched on at request time, which is exactly the property that makes "it works
for R2" *suggestive* for B2 rather than *evidence* for it.

What is actually established: B2 passes the conformance suite against
`LocalStore`, and its configuration is constructed by the same code that R2's
is. What is not established: that B2's endpoint accepts these requests, that its
`head` returns the size field this code reads, that its ETag shape does not
break anything, that range requests clamp rather than 416, and that its
delete-idempotence matches the trait contract. Every one of those is a place
where S3-compatible implementations are known to differ.

D-05 records B2 as *viable behind the same trait*. That claim is currently
untested against the thing it is a claim about. It should stay open until
someone runs `bucket_roundtrip` against a live B2 bucket — the same one-command
step the R2 acceptance was, now with `--keep` so the result can be checked
independently (F-61).

**Consequence of leaving it open:** the honest description of the storage layer
is "R2-verified, B2-plausible", and any roadmap or README claiming B2 support
is overstating what has been demonstrated.

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
| 2026-08-22 | D-08 | Opened — base-image hosting; four options, recommendation (c)-then-(b), **not decided** (Claude Code) |
| 2026-08-22 | D-09 | Opened — B2 implemented but never run against a live bucket; tracking item (Claude Code) |
| 2026-08-22 | D-08 | **Resolved** — GHCR by digest + local-cache fallback + `export --with-base-image`; recommendation (c)-then-(b) superseded (Rain) |
| 2026-08-22 | E-13 | Opened — authenticating on a second machine revoked the first's credential; contradicts an assumption D-02 rests on (Rain) |
| 2026-08-22 | F-12 | **Scheduled** — confirmed in production during the cross-machine validation; recovery required delete+recreate (Rain) |
| 2026-08-22 | D-11 | Opened — no user-facing path moves a bundle through storage; sync layer is commercial scope (Claude Code) |
| 2026-08-22 | D-12 | Opened — import needs a pre-existing project and an invented quota the bundle already records (Claude Code) |
| 2026-08-24 | E-15 | Auth ruling (2d): Codex authenticates as Claude Code; AUTH agent-agnostic. Codex marked implemented-but-unverified (F-84); Gemini recorded as next verified target (Product Owner + Claude Code) |
| 2026-08-24 | F-85 | Opened — changing the base image under a fixed version orphans bundles referencing the old digest; needs versioning-discipline ruling (Claude Code) |
| 2026-08-24 | F-85 | **RESOLVED** — version the image (0.2.0 two-agent; 0.1.0 kept), per-version immutable digest ledger enforced at build/publish/CI, D-08 advice names the bundle's version (Product Owner ruling) |
| 2026-08-23 | E-15 | Resolved in code — one agent per project, recorded and switchable; Codex added; session-state capture blocked on Codex auth (Claude Code) |
| 2026-08-23 | D-10 | Re-estimated after publishing: still 3–5 days; auth row mostly gone, digest verification newly mandatory, urgency dropped. Recommend leaving the ruling (Claude Code) |
| 2026-08-23 | D-08 | Part 1 delivered — published to GHCR from CI with a digest-divergence check; base image renamed to ghcr.io/gnrain/nemr-base (Claude Code) |
| 2026-08-23 | E-14 | **Resolved** — restore defers the credential check, `create` keeps AUTH-03; AUTH-03 reworded to state the distinction (Rain) |
| 2026-08-22 | D-10 | Opened — registry pull out of D-08 scope; tracked as a GUI prerequisite, Transfer service, 3–5 day estimate (Claude Code) |
| 2026-08-22 | D-08 | Part 1 scoped down (Rain): nemr does not pull; error states the limit and gives the exact `ctr` command |
| 2026-08-22 | E-10 | **Deferred with direction** — Linux first, then WSL2, then a bundled VM on macOS; remote-engine fallback rejected. Moved out of Open (Rain) |
