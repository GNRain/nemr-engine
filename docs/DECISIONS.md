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

**Revised mechanism — (f), the credential mount is read-write (2026-09-05,
Product Owner ruling; SPEC 1.102).** The substance is unchanged: per-device,
never synced, never in a bundle, injected from the host. What changes is that
the session may now *write* the one file it mounts, because Claude Code renews
its login by rewriting it, and a read-only mount made every session's login
die at the access token's eight-hour expiry with an error blaming revocation
(F-130). Established before building, on the reference host with the real
credential: a session refreshed an expired access token through a writable
single-file bind, in place, and the host continued on the rotated pair.

*The enumeration the ruling asked for — what a session can now write.* On a
real host `~/.claude` holds: `.credentials.json` (the credential; the only
thing mounted); `history.jsonl` (prompt history across every project — 190 KB
here; must never be mounted); `projects/` and `sessions/` (session state — the
M8 binds from the volume shadow these); `settings.json` (preferences — the
F-131 question, not this one); and `backups/`, `cache/`, `debug/`, `plugins/`,
`shell-snapshots/`, `daemon*`, `stats-cache.json` and similar machine-local
state. Identity — `machineID`, `userID`, `oauthAccount` — lives in
`~/.claude.json`, *outside* the directory, on the rootfs, never mounted (F-54).
**So: not the whole directory.** One file becomes writable; identity files were
never in the mount and stay read-only where they are; everything else stays
container-local. A whole-directory bind would share the host's history in both
directions and need a blocklist that fails open on the next Claude Code
release (the F-54 argument).

*The exposure, stated honestly.* Every process in a session — the agent
included — could already read the credential (root, mode 600). Now it can
write it: refresh it (intended), blank it (Claude Code does so itself after a
dead refresh), or overwrite it, which would put the host's own Claude Code on
whatever the session wrote. That is a same-user boundary, the same one the
daemon socket relies on; it is why the file stays the *only* writable thing
from `~/.claude`, and why nothing here touches what travels.

*F-12, diagnosed and bounded.* The container's Claude Code writes the file in
place — a rename over a mount point cannot succeed, so it falls back — and the
host sees every session-side refresh. The host's Claude Code writes by rename,
so after a *host-side* refresh a running session's bind still shows the
previous file, whose refresh token has been rotated away, and its next refresh
fails and blanks its own copy. Remedy: `nemr stop && nemr start`, which
re-resolves the path (proven on a real project); not delete-and-recreate.
Detection: `status` and `attach` compare device and inode of the task's view
(`/proc/<pid>/root`) with the host's and say STALE, with that remedy. What a
session cannot recover from by itself is now exactly three states — a spent
refresh token, a blanked file, a stale mount — and those are the only ones
reported; an eight-hour access expiry is routine and silent.

*What existing projects do.* Their records carry `ro`; `start` repairs the
record before the task exists (the NET-02 migration's shape, with the F-112
lesson: a read-only control on the spec, a test seam for the old shape, and
proof from the running task's `mountinfo`).

**The posture, stated plainly (2026-09-05, the Product Owner's condition;
SPEC 1.103).** The container can refresh the credential. The container can
overwrite it. The overwrite is observed. The mount cannot distinguish the two,
and it cannot be made to without breaking the refresh, so what a session does
to the host's login is made visible instead — the principle of the privileged
helper's audit trail: anything with the power to break the host gets a record.
The daemon watches the host file and records every rewrite — when, which
sessions could have made it (by elimination, and it says so: an in-place
write is a session's, a replacement is the host's), and whether the result
parses as a credential or as junk — in its log as an audit line and on
`nemr status` as `last rewrite`. It could not go through the per-request
audit stream, which is correlated by request id; a session's write happens
outside any request. And F-12 is dead rather than bounded: a host-side
replacement is re-bound into every running session the moment it lands, and
again before every attach, so a session left running past the eight-hour
window keeps working whether it or the host refreshed first. Not prevented,
still: a session can lock the host out of Claude Code by writing junk; the
record names it, and the host logs in again.

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

### D-13 — Node.js and Claude Code are prerequisites, not things nemr installs

**Status:** Resolved · 2026-09-03
**Raised by:** the WSL2 spike (divergence 3) · **Decided by:** Product Owner ·
**Relates to:** NFR-01, E-17, E-10

**Question.** `setup_host.sh` provisions the engine, then expects `claude` to
exist for the credential step. The WSL2 spike ran on a host where Claude Code and
its Node runtime were absent, and a global `npm install -g` failed with EACCES.
So: should `setup_host.sh` install Node and Claude Code — via NodeSource's apt
repo, say — or only detect their absence and instruct?

**Ruling (Product Owner).** Detect and instruct. nemr does **not** add a
third-party apt repository (NodeSource) and does **not** install Node or Claude
Code. They are stated prerequisites the user provides, exactly as the GPU/CUDA
host is treated (E-10) — the project names what it needs rather than reaching
outside the distribution archive to conjure it.

**Rationale.** This is NFR-01 applied one layer out. NFR-01 forbids depending on
Docker anywhere we build, ship, script or **request** (E-17); the same principle
forbids `setup_host.sh` silently wiring a user's system to a third-party apt repo
and running a global `npm` install as a side effect of "provision the engine". A
prerequisite the user installs deliberately, from a source they chose, is honest;
a provisioner that reaches out to NodeSource on their behalf is the quiet
supply-chain expansion NFR-01 exists to prevent. It also keeps the failure
legible: "Claude Code is not installed, here is how" beats an opaque `npm EACCES`
mid-provision.

**Consequences.**
- `setup_host.sh` gains a detect-and-instruct step before the credential step: it
  reports whether `claude` (and `node`) are present and, if not, points at the
  install instructions — non-fatal, because the engine and its test suite
  provision fully without them (they use fixtures and a placeholder credential).
- PREREQUISITES.md records Node + Claude Code as prerequisites with the install
  pointer, alongside the existing host prerequisites.
- Not done, deliberately: no NodeSource repo, no `npm -g` run by the script, no
  bundled Node. If a future installer (the D-01/E-09 product path) needs to ship
  a runtime, that is its own decision, made explicitly — not inherited here.

---

## Deferred (decided, with direction — not gaps)

A deferred entry is a **decision to sequence work later**, with the direction
settled so nothing downstream is designed against the wrong assumption. It is
not an Open item: Open means unresolved.

### E-10 — Non-Linux hosts

**Status:** **Windows half resolved — WSL2** (2026-09-02); macOS stays
deferred; remote-engine fallback stays rejected. Was Deferred with direction
(2026-08-22), Open since the first brief.
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

**Partial resolution — Windows means WSL2 (2026-09-02, Rain).** A deliberate
platform decision, recorded rather than made by accident.

Windows support is Nemr running **inside WSL2** — not a native Windows binary,
not a bundled VM. The stack (rootless containerd, loopback ext4, user
namespaces, veth and NAT) is Linux kernel features, and WSL2 is Microsoft's own
Linux kernel with filesystem, network and (later) GPU integration already
solved. Docker Desktop, Podman Desktop and Rancher Desktop all arrived at the
same answer for the same reason. The product claim: a Windows user installs
WSL2, runs `setup_host.sh` inside it, and works from that terminal — stated
plainly in the README so nobody expects a `.exe`. Explicitly out of scope:
macOS (still deferred), GPU (a separate future pass, dependent on this one
succeeding — one unknown at a time), and any Windows-native code.

**Why the sequencing changed** — recorded so the chain is visible rather than
reconstructed later: Rain needs a GPU test host; the only NVIDIA card available
is in a Windows machine; VirtualBox cannot pass it through; there is no disk to
spare for dual-boot. WSL2 on that machine is the path to a GPU host, and the
Windows half of E-10 comes with it.

**Execution — establish, then support, strictly in order.** Half 1 is a spike:
`setup_host.sh` and `verify_wp_a.sh` on a fresh WSL2 Ubuntu, every divergence
from the Ubuntu-VM baseline recorded, no code adapted before knowing what
actually breaks (see `docs/wsl2-spike.md`). Half 2 fixes only what the spike
proved broken, narrowly — WSL2 detection where the path genuinely differs, not
a fork of every script. Acceptance mirrors the cross-VM run: export a session
on a Linux box, import on WSL2, attach, and continue the conversation with
history intact; then the reverse. Plus the two checks only a person can make: a
dev server inside a session reachable from a Windows browser, and
`verify_wp_a.sh` green on WSL2 against installed binaries.

**Accepted cost — decided rather than discovered.** WSL2 becomes a second
supported host: `setup_host.sh` grows WSL2 awareness, PREREQUISITES.md gains a
section, and every host-backed test acquires a "does it hold on WSL2?"
question. GitHub's hosted runners cannot run WSL2, so WSL2 verification is
manual, by Rain, until something better is found. **That permanent maintenance
cost is exactly what deferring E-10 was avoiding, and it is accepted knowingly,
not unnoticed.**

---

### E-18 — A local LLM on the host GPU for a session's agent (Phase 4)

**Status:** **Deferred, with direction** · 2026-09-05
**Raised by:** Product Owner (2026-09-03), once E-10 made the only GPU host
reachable · **Ruled by:** Product Owner · **Relates to:** E-10, D-02, D-13,
NET-02/NET-05, NFR-01

**Question.** Can a session's agent work against a model served from the
host's GPU, with no API call leaving the machine — and what would the engine
have to grow to do it?

**What the spike established** (`docs/gpu-spike.md`,
`docs/gpu-phase2-ollama.md`, `docs/gpu-phase3-agent.md`; three phases by hand,
nothing built, no CI ever — hosted runners have no GPU). Phase 1: a rootless
container on our containerd uses the RTX 3070 through **three bind mounts and
one env var** (`/dev/dxg` rw, `/usr/lib/wsl/lib` ro, the hashed driver store
ro, `LD_LIBRARY_PATH`) — no device entries, hooks, CDI or cgroup rules, so it
fits today's `ContainerSpec`. Phase 2: an 8B model fully resident at 74 tok/s
through that delta unchanged. Phase 3: Ollama's Anthropic-compatible API is
correct and a real session reaches it at its gateway; **but tool calling under
a real agent's toolset fails everywhere** — three models (7B dense to 30B
MoE), two agents (Claude Code, Codex), two servers (Ollama 0.33.3, llama.cpp's
own server), three endpoints, two GGUF sources, the parser forced. The models
emit correct calls as text; no layer converts them. Known upstream for Ollama
(ollama/ollama#15529, closed unfixed); reproduced on llama.cpp at the
single-tool baseline. Both are drafted as upstream reports in
`docs/gpu-upstream-issues.md`, evidence-first, for the Product Owner to file.
And the ceiling the runbook fixed before its first run: **8B on 8 GB is a
plumbing proof, not a usable assistant** — even with tool calling fixed, the
models that fit this card are not coding agents.

**Ruling (Product Owner, 2026-09-05).** Phase 4 — the engine work — is
**deferred, not cancelled.** The delta is known and small. It reopens when
either changes: the translation layer is fixed upstream, in a server we would
run as a dependency (verified by re-running Step 7c then 7d of the Phase 3
runbook — a trivial tool call, then a completed edit in an empty directory);
or there is a card that runs a model that matters. The feature waits on
hardware as much as on software. A translation proxy stays a fallback on
paper, not a plan: the ones that make text tool calls work are per-model text
parsers for undocumented formats, the class this project has eliminated
(F-58's lesson, one layer up).

**Settled Phase-4 inputs — measured, do not re-test:**

1. **A session reaches a service in rootlesskit's namespace at its own
   gateway** (`10.99.N.1`, from `ip route` inside the session), through
   NET-02 with NET-05 intact — the isolation is a `FORWARD` drop, delivery
   to the gateway is `INPUT`. No networking change is needed.
2. **`ANTHROPIC_BASE_URL` wins over the mounted credential.** With the real
   D-02 credential present and egress available, the negative control failed
   to connect rather than reaching `api.anthropic.com`. A GPU session can
   carry the user's credential and point at a local model without the two
   colliding — the design question prediction 4 would have opened is closed.
3. The GPU contract above, and the driver-store directory **discovered at
   start** (`/usr/lib/wsl/drivers/nv_dispi.inf_amd64_<hash>` changes on every
   Windows driver update), never hardcoded.
4. Context: 8192 fully resident on this card; 16384 at 87% residency and
   ~40% throughput cost. A trade-off to expose, not a wall.
5. Lifecycle: `--rm` does not clean up a GPU container after SIGKILL; a model
   cache belongs on a bind (llama-server's download otherwise lives in the
   writable layer); `nemr status` should surface the session's gateway
   address; `claude --bare` skips the `count_tokens` pre-flight; the agent
   does not know its real context window.
6. **Not needed:** `nvidia-container-toolkit`, any third-party apt repo, CDI,
   `nerdctl`. The D-13-class question the Phase 1 runbook flagged never
   arises.

**Not done, deliberately:** no proxy or parser; no `setup_host.sh`, engine,
base-image or containerd-config change; no upstream fix attempted; nothing
with CI coverage, and nothing in this line ever will have it.

---

## Open

### F-131 — First-run onboarding repeats in every fresh session: preferences live on the rootfs

**Status:** Open · **raised, not built** · 2026-09-05
**Raised by:** Product Owner (WSL2 daily use) · **Relates to:** F-54, D-02, WP-C

**What was observed.** Every fresh session opens Claude Code with the theme
picker and the syntax-theme step, as if the user had never run it. Cosmetic,
but it is the "why does this session feel new every time" experience, and it
is separate from the credential (F-129/F-130).

**Why.** Claude Code keeps first-run state and preferences in the *user*
config (`~/.claude.json` fields such as `hasCompletedOnboarding` and `theme`,
and `~/.claude/settings.json`), which lives on the container rootfs — not on
the volume — because WP-C classified the user config as identity-bearing
(`machineID`, `userID`, `oauthAccount`) and kept it off the portable layers
(D-02). That classification is right for identity and wrong for a theme. F-54
already made this exact distinction once, for MCP configuration, and resolved
it *structurally*: project-scoped `.mcp.json` lives at the project root, which
is the volume, so it travels without any field filtering.

**The question, for a ruling.** Does a portable-preferences subset exist, and
where does it live? Three shapes, none chosen:

| # | Shape | What it buys | What it costs |
|---|---|---|---|
| a | **Seed, don't sync**: at create, copy a small allowlist of preference fields from the *host's* `~/.claude.json` (theme, onboarding-complete) into the session's rootfs config | No onboarding on this host's sessions; nothing travels in a bundle, so D-02 is untouched | Host-specific: a session pulled onto another machine onboards once there. Needs the allowlist to be maintained as Claude Code's schema moves (the F-54 drift-warning pattern) |
| b | **Travel on the volume**: a preferences file on the volume, pointed at with `CLAUDE_CONFIG_DIR` or a settings path | Preferences follow the session across machines | `CLAUDE_CONFIG_DIR` relocates the *whole* config directory including the credential — WP-C2 rejected exactly this on D-02 grounds. Only viable if Claude Code offers a preferences-only path, which must be established, not assumed |
| c | **Leave it** | Nothing to maintain | The first-run flow on every session, forever; the UI's "click one, it comes down, attach" lands on a theme picker |

**What must not happen:** any shape that moves `~/.claude.json` wholesale.
The identity fields are the reason the file stays put, and F-54's allowlist
(portable: `mcpServers`, `projects`; everything else stays) is the measured
boundary. A preferences subset would extend that allowlist by named fields,
not replace it.

**Recommendation, held loosely:** (a) — seed at create from an allowlist, no
sync — is the smallest change that removes the daily annoyance without
touching D-02 or the bundle format, and it degrades to today's behaviour on a
foreign host. It waits on a ruling and on one measurement: the exact fields
Claude Code reads for onboarding state and theme in the installed version,
verified by reading them, not remembered.

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

**Ruling — the HTTP surface (2026-09-06, Rain).** The UI's HTTP surface is
served by the **commercial process, as a gRPC client of the daemon**. The
daemon keeps its Unix socket and serves no HTTP; **no port opens unless the
user starts the UI**. Decided on the spike (`docs/http-spike.md`: a second
listener is routine wherever it lives) and on E-11's own test: the UI needs
an account, so every route on it is commercial, and the open half loses
nothing. The daemon-side seam the commercial process talks through — the
proto and connect code as a small open crate — was referred to as
`nemr-daemon-api`, "approved as landed", when it did not exist anywhere on
origin (checked 2026-09-06; the phantom-report rule). It was then built,
under that name, as SPEC 1.105: the engine re-exports it, the open CLI and
the commercial process both reach the daemon through it, and the handshake
had been built before it without depending on it.

**Ruling — what is commercial (2026-08-20, recorded in SPEC 1.31).** Engine +
wrapper + volume layer + privileged helper + **bundle format spec** are open.
Sync, lease, storage backends, identity, and the client-side encryption are
commercial. The test: *can someone use the open half productively without ever
paying?*

**Ruling — packaging shape (2026-08-26).** The commercial code is a **crate in
the workspace**, not a sibling repo. The seam is `scripts/check_seam.sh`, which
proves at the dependency-graph level (plus a source grep, plus a self-contained
control) that the open half reaches no commercial crate. That is isolation by
*evidence*; a sibling repo isolates by *absence*, which is unfalsifiable — no
test can fail on it — and this project isolates by evidence everywhere else.
Two further reasons: the server shares the open bundle/manifest/digest types by
path (dependency flowing commercial → open, the allowed direction), which a
repo split would force into duplicated-or-published crates, taxing every
bundle-format change; and crate → repo is a mechanical extraction of a
self-contained directory, whereas repo → shared-crate is a merge of drifted
types, so crate-first keeps the cheaper reversal open. **Licensing is upstream
of packaging and remains unchosen** (no `LICENSE` file; `publish = false` on
every crate). If the server source must later be closed, extraction is cheap
*because* it is already a clean crate. Commercial crates so far:
`crates/nemr-storage` (M12), `crates/nemr-crypto` (E-16), `crates/nemr-sync`,
and `crates/nemr-cloud` (WP-K).

**How commercial commands reach the open CLI without breaching the seam
(2026-08-26, WP-K).** The open `nemr` gains one generic mechanism — cargo/git
external subcommands: `nemr <unknown> …` execs `nemr-<unknown> …` from PATH.
The commercial client is one binary installed under several names
(`nemr-login`, `nemr-push`, …) with argv[0] dispatch. So `nemr login` works end
to end, while the open CLI carries no extension names and no dependency on any
extension — the seam grep continues to prove no commercial name appears in the
open tree, and `nemr export`/`import` keep working with no network, no account,
and no extension installed. The alternative — subcommands compiled into the
open CLI behind a feature flag — would have put commercial names and wiring
into the open tree, exactly the erosion E-11 exists to prevent.

---

### E-16 — Encryption key origin

**Status:** Resolved · 2026-08-26
**Relates to:** D-02, D-03, E-11, E-13, D-06 · **Implemented in:** `crates/nemr-crypto`

**Question.** The server stores ciphertext it cannot read, yet a user must log
in on a new machine with only email + password and decrypt their sessions.
Those pull in opposite directions: the key must be recoverable from what the
user carries between machines, but never derivable by the server that
authenticates them. Where does the key come from?

**Ruling — Option B, a wrapped master key.** A random 256-bit **master key
(MK)** is the data key, generated once at registration. It is wrapped by a
**password-derived envelope** — `AEAD(kdf(password), MK)` — which the server
stores as an opaque blob. Login on any machine fetches the envelope, derives the
wrap key locally, and unwraps MK. The server never sees the password's wrap key
or MK.

The decisive property is decoupling: because MK is random and merely *wrapped*
by the password, every future access method — GitHub OAuth, per-device caching,
recovery — is **another envelope over the same MK, with no re-encryption of any
bundle**. Deriving the data key straight from the password (the rejected Option
A) would make the first added login method a re-encrypt-everything event.

**Mechanism (all standard primitives, no invented constructions).** One
Argon2id over `(password, salt)` yields a root; HKDF-SHA256 with distinct `info`
labels splits it into an `auth_key` (sent to the server) and a `wrap_key` (never
leaves the client) — domain separation is what lets authentication travel
through the server while the data key does not. Wrapping and bundle encryption
use XChaCha20-Poly1305 (random 192-bit nonce, no counter to manage). Argon2id
parameters are stated and justified in SPEC §crypto and stored per-user so cost
can be raised for new accounts without invalidating old ones.

**D-02 is a different secret — record it in these terms.** D-02 governs
Anthropic's credential: **someone else's secret, per-device, which never
travels.** E-16 governs **the user's own bundle key, which must travel — as
ciphertext the server cannot open — or the product does not work.** Same word
"key", opposite requirement; they do not conflict. Reading both without this
distinction, one would assume they contradict.

**Recovery is not deferrable (Product Owner ruling, overriding the initial
proposal to defer it).** The recovery envelope is generated **at registration**,
the recovery code is shown once, and the account is **not usable until the user
confirms** they stored it. The confirmation is server-enforced without the
server learning MK: registration records `SHA-256(domain || MK)`; to confirm,
the client recovers MK from the recovery envelope with the re-entered code and
recomputes the hash — a match proves recovery actually recovers the *same* MK.
Deferring recovery to a later settings flow would leave every bundle created
before that flow unrecoverable, and the silent, permanent failure would land on
the users least likely to have gone looking for it.

**Cost, accepted explicitly (Product Owner's words).** *A forgotten password
with no recovery envelope means the user's bundles are unrecoverable,
permanently. There is no server-side reset that preserves data, because the
server cannot read the data. That is inherent to real client-side encryption,
not a shortcoming of this design — and it is the price of the server being
unable to read one byte of anyone's source code or transcripts.*

**Per-device caching** is a layer, not a fork: after first unwrap, MK can be
cached in the OS keychain wrapped by a device key, so the password is needed
only at first login on a device and revoking a device deletes its envelope. The
envelope table is built so this is addable; it is **not** built in this pass.

**Does not resolve E-13.** E-13 (authenticating on a second machine revoked the
first's credential) concerns Anthropic's OAuth credential — D-02's secret, not
MK — so it is orthogonal and stays open on its own terms.

---

### E-17 — Does NFR-01 reach the CI provider's own substrate?

**Status:** Resolved · 2026-08-27
**Raised by:** Claude Code (from a constraint check firing) · **Relates to:** NFR-01, R-03, F-94

**Question.** The Docker-freeness gate flagged a comment stating that GitHub
Actions service containers are started by Docker. Reading what the comment
claimed rather than silencing it raised a real question: our CI asked for a
`services: postgres` block, so does NFR-01 — "no component of the stack may
depend on Docker Engine, directly or transitively" — cover it? And if it
covers that, does it also cover the Docker daemon the runner image ships?

**Established first, not argued.** From our own job log, the runner executes
`/usr/bin/docker version`, `docker network create`, `docker pull postgres:16`,
`docker create --name …_postgres16_… -p 5432:5432 … postgres:16`, and
`docker start`. Those commands run **because our workflow file asked for
them**; delete the block and they do not.

**Ruling.** NFR-01 covers everything we build, ship, script, or **request**,
including the test harness and CI configuration. A `services:` block is a
request, so it is in scope and it was a violation. NFR-01 does **not** cover
the Docker daemon preinstalled on runner images.

**Rationale.** The line is fine but real: we control what we ask for, and we do
not control what the provider preinstalls. The deciding argument is that a
constraint reaching the substrate would be **unsatisfiable on every
GitHub-hosted runner** — and a constraint that cannot be satisfied is not a
constraint, it is a permanent violation everyone learns to ignore. That is
worse than a written exclusion.

**Why it is written into the spec rather than left understood.** The scope had
been resting on an inference — "whoever wrote the gate already scanned
`.github`, so CI must be in scope". Unstated scope is how a hard constraint
quietly becomes unenforceable in one direction and unsatisfiable in the other.

**Consequences.**
- The `services: postgres` blocks are replaced by rootless podman, which is
  daemonless and does not use the system containerd — so it also survives
  `ci_provision_host.sh` disabling that containerd under PRIV-01, which is what
  killed the database in the host job (F-93). One mechanism, both jobs, and the
  same mechanism developers run locally.
- The gate is widened to catch `services:`/`image:` (F-94). Widening is always
  safe; narrowing needs a ruling.
- Nothing else about NFR-01 is softened: Docker at any layer we build, ship,
  script or request remains release-blocking.

**Process note, recorded because the shape recurs.** The first response to the
check firing was to reword the comment so it stopped matching. That kept the
build green and made a real finding invisible; it was recoverable only by
re-reading the edited prose. **When a constraint check fires, the first
question is what it found, not how to make it pass.**

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

**Status:** **Mechanism resolved** (2026-09-05, Rain, on E6) — refresh-token
rotation, observed directly; the cross-machine revocation is the same
mechanism seen from two machines holding copies of one refresh token. No
longer a daily cost: a session shares the host's live file (D-02 (f)) and a
host-side refresh reaches it through the watcher's re-bind. Was Open —
escalation, raised for understanding before design.
**Raised by:** Rain (cross-machine validation) · **Contradicted:** D-02's assumption
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

> **Investigation (2026-09-05, Claude Code — evidence, not a ruling).** Raised
> again by the Product Owner after it became a daily cost: three in-container
> logins on the WSL2 box, each browser-validated, each followed by
> `401 OAuth access token has been revoked`. What can now be established from
> primary sources, and what cannot:
>
> **1. The token model, from the file and the docs.** A subscription login is
> stored in `~/.claude/.credentials.json` (mode 0600 on Linux) as
> `claudeAiOauth: {accessToken, refreshToken, expiresAt, refreshTokenExpiresAt,
> scopes, subscriptionType, rateLimitTier}`. Measured on the reference host:
> the **access token lives 8 hours** (file mtime → `expiresAt`, 8.0 h), the
> **refresh token about two weeks** (`refreshTokenExpiresAt`). Claude Code
> refreshes the access token with the refresh token and rewrites the file. The
> docs describe the login lifetime and a startup warning three days before it
> ends ("Renew an expiring login"); `/logout` "removes and revokes the
> credential this sign-in wrote."
> ([docs: Authentication](https://code.claude.com/docs/en/authentication))
>
> **2. What the docs do NOT say.** Nothing in the authentication or error
> reference states a per-device limit, a concurrent-session limit, or that a
> login on one machine revokes another's. The error reference lists
> `OAuth token revoked` with the cause "revoked or expired" and the single
> remedy `/login` ([docs: Errors](https://code.claude.com/docs/en/errors)). A
> policy of "one machine at a time" is **not documented**; the observation
> stands, its cause does not.
>
> **3. What the community record suggests — refresh-token rotation, not a
> device policy.** Issue
> [anthropics/claude-code#54443](https://github.com/anthropics/claude-code/issues/54443)
> (2.1.121, Linux, Max; closed stale, no maintainer answer) reproduces
> cascading `/login` prompts across *two concurrent sessions sharing one
> credentials file*, with the token endpoint returning 400 on refresh hours
> before the local `expiresAt`, and reasons that refresh tokens are rotated
> and single-use: a stale refresh from one holder invalidates the family for
> the other. That mechanism fits our observation exactly, with no device
> policy needed: two machines holding copies of one refresh token cannot both
> refresh; the loser is "revoked." The "revoked immediately after login"
> class is a recurring bug with no stated cause
> ([#13350](https://github.com/anthropics/claude-code/issues/13350),
> [#29497](https://github.com/anthropics/claude-code/issues/29497), both
> closed without explanation). Separately,
> [#53063](https://github.com/anthropics/claude-code/issues/53063) reports
> that a non-interactive `claude -p` does not refresh at all and fails after
> the 8-hour expiry. **This is where the daily pain actually lives: a
> read-only mount (D-02) can never persist a refresh, so a session's
> credential is dead within 8 hours of the host's last refresh regardless of
> any other machine.** That is F-12's problem, sharpened: rotation on the host
> is invisible in the container *and* the container cannot rotate for itself.
>
> **4. An option the first table did not have — (e).** The docs describe
> `claude setup-token`: a **one-year OAuth token** for "CI pipelines, scripts,
> or other environments where interactive browser login isn't available,"
> read from the `CLAUDE_CODE_OAUTH_TOKEN` environment variable (precedence
> rank 5, above the `/login` credential). It "authenticates with your Claude
> subscription," "can only make model requests," and is self-managed — no
> on-disk rotation for a read-only mount to defeat. Injected per device as an
> environment variable rather than a file, it fits D-02 unchanged (per-device,
> never synced, present at attach) and sidesteps the read-only-file problem
> structurally. Two caveats from the same page: **bare mode does not read
> it** (`--bare` needs `ANTHROPIC_API_KEY` or an `apiKeyHelper`), and a
> one-year token is a longer-lived secret than an 8-hour one — the same
> trade option (b) named, but for a *subscription* token rather than an API
> key. Whether it is also subject to whatever revoked the WSL2 credential is
> exactly what the experiment must establish.
>
> **5. What would settle it now — two experiments, both cheap, neither run.**
> (i) The one already proposed: authenticate on A, confirm; authenticate on
> B; re-test A — and this time read `refreshToken` (hashed) on A before and
> after, so rotation is observed rather than inferred. (ii) A session
> carrying `CLAUDE_CODE_OAUTH_TOKEN` from `claude setup-token` instead of the
> file mount, left past the 8-hour mark and past a login on another machine.
> If (ii) survives both, option (e) is the design; if it does not, the
> revocation is account-level and (a) is the honest answer.
>
> **Recommendation, unchanged in kind: (d), then decide.** The two immediate
> costs are addressed without waiting: `nemr status` now reports the expiry
> (F-129) and `nemr attach` names the state before Claude Code's login prompt
> can mislead (F-130). Nothing here designs around E-13; it only replaces
> "one event, unexplained" with a mechanism the evidence supports and a test
> that would confirm or kill it.

> **Experiment, Part A run (2026-09-05, Claude Code, reference host) —
> `docs/e13-token-spike.md`.** Precedence is settled: with a fake env-var
> token and a fake credential file bound read-only exactly as the engine binds
> the real one, Claude Code 2.1.240 in the base image sent the **env-var
> token** as its bearer in every file state — expired file present, valid
> file present, no file — and its debug log never touched the file path in
> those runs. The control without the env var reproduced F-130's mechanism
> verbatim: `OAuth refresh failed (expected): 400` → `OAuth dead-token disk
> clear: backend write failed` → the expired token sent anyway. `--bare` does
> not read the env var (no request at all). Exposure measured and stated:
> the env token is readable by every process in the session (`/proc/1/environ`,
> `env`) and sits in the container record on the host — and the mounted file
> is readable by the same processes today (root, mode 600). The change is the
> secret's lifetime (one year versus eight hours plus a two-week refresh the
> container cannot use) and its scopes (`user:inference` only). Part B — the
> real token's issuance, acceptance, survival past eight hours and past a
> login elsewhere — is the Product Owner's on the WSL2 box; the ruling waits
> on it, and must also choose create-time versus attach-time injection
> (exec-time injection needs no container-record change and is D-02's own
> wording) and whether the file mount stays for the non-Claude agents.

> **Ruled (2026-09-05): option (f), not (e).** The Product Owner chose to make
> the mounted credential writable so the session refreshes it, rather than a
> long-lived token — see D-02's revised mechanism. Established in the same
> session: refresh tokens **rotate on every refresh** (both tokens changed on a
> real refresh, twice), which is the mechanism this entry inferred; and the
> previous access token kept working after the refresh. The cross-machine
> question stands as inferred — two machines holding copies of one refresh
> token cannot both refresh — and is no longer a daily cost, because a session
> now shares the host's live file rather than a read-only snapshot of it.
> Part B of the token spike is not needed for the ruling; it remains a valid
> experiment if (e) is ever wanted for headless paths (`--bare`, Codex).

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
| 2026-09-02 | E-10 | **Windows half resolved — WSL2** (no native binary, no bundled VM); macOS stays deferred, remote fallback stays rejected. Sequencing reopened for a GPU test host; permanent manual-verification cost accepted (Rain) |
| 2026-09-03 | D-13 | **Resolved** — Node and Claude Code are prerequisites nemr detects and instructs for, never installs; no NodeSource apt repo, no `npm -g` by the script (NFR-01 one layer out; WSL2 spike divergence 3) (Rain) |
| 2026-09-05 | E-18 | **Deferred with direction** — a local LLM on the host GPU: the contract is three binds + one env var and a session reaches the server at its gateway with the credential override holding, but tool calling fails under a real toolset in every server/model/agent combination (upstream, drafted in `docs/gpu-upstream-issues.md`); 8B on 8 GB is a plumbing proof, not an assistant. Phase 4 reopens on an upstream fix or a card that matters; the two Arm B findings are settled inputs (Rain) |
| 2026-09-05 | E-13 | **Investigated** — the token model measured (8 h access, ~2-week refresh); no documented device/session limit; the community record fits refresh-token rotation, not a policy; the read-only mount means a session cannot refresh at all (F-12, sharpened); option (e) `claude setup-token` as a per-device env var, with two experiments to settle it. Still Open (Claude Code) |
| 2026-09-05 | F-131 | **Opened** — first-run onboarding repeats every session because preferences share the identity-bearing user config on the rootfs; three shapes tabled, (a) seed-from-allowlist recommended; raised for a ruling, not built (Claude Code) |
| 2026-09-05 | E-13 | **Experiment Part A run** — env-var token wins over the mounted file in every state incl. expired-present; F-130's mechanism confirmed in Claude Code's debug log; `--bare` ignores the env var; exposure measured (readable in-session like the file today; in the host container record). Part B (real token, 8 h, login elsewhere) is Rain's; ruling waits (Claude Code) |
| 2026-09-05 | D-02 | **Mechanism revised — (f): the credential mount is read-write** so the session refreshes its own login; substance unchanged (per-device, never synced, never in a bundle). Enumeration: one file, not the directory; identity never in the mount. Exposure stated (read was already possible; write is new; same-user). F-12 bounded: in-place write from the session, rename on the host; stop/start is the remedy; STALE detected by inode. Existing records migrated at start (Rain; SPEC 1.102) |
| 2026-09-05 | E-13 | **Ruled (f) over (e)**; refresh-token rotation observed directly (both tokens change on every refresh; the prior access token survives). Cross-machine mechanism stands as inferred; no longer a daily cost (Rain) |
| 2026-09-05 | D-02 | **Condition met — the overwrite is observed, F-12 dies.** Credential watcher in the daemon: every rewrite attributed and parsed, logged and on `status`; host-side replacements re-bound into running sessions live and before attach (SPEC 1.103) (Rain's condition; Claude Code) |
| 2026-09-05 | F-12 | **Closed** on E6 — the 8-hour acceptance: a session left running past the window, host refresh by rename meanwhile, answers `claude -p`; re-bound by the watcher; one inode both sides (Rain) |
| 2026-09-05 | E-13 | **Mechanism resolved** — refresh-token rotation, observed; cross-machine revocation is the same mechanism; no longer a daily cost under D-02 (f) (Rain, on E6) |
| 2026-09-06 | E-11 | **HTTP surface ruled** — commercial process, gRPC client of the daemon; daemon keeps its socket; no port unless the user starts the UI. `nemr-daemon-api` referred to as landed: not on origin, reference requested (Rain; recorded by Claude Code) |
| 2026-09-06 | E-11 | **`nemr-daemon-api` built** — the daemon's proto, stubs, socket path and client as an open crate; engine re-exports; freshness gate and seam check extended to it (SPEC 1.105) (Claude Code) |
