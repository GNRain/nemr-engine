# E-13 option (e) — a `claude setup-token` in the session, instead of the mounted file

The question, from the Product Owner after merging F-129/F-130:

> Two things to establish: does `claude setup-token` issue what the docs
> describe, and does Claude Code inside a session use an env-var token in
> preference to a mounted `.credentials.json`? If the file's checked first and
> found expired, the env var may never be reached — test with an expired file
> present, not just absent.

Same discipline as the GPU spike: by hand, **build nothing**, evidence first.
Two halves. **Part A** — precedence — was run on the reference host with fake
tokens against a capture server, so it spent no real credential and proves
*which* credential Claude Code sends, not whether Anthropic accepts it.
**Part B** — the real token's issuance, lifetime and survival — needs a
browser approval and hours of wall clock, and is the Product Owner's, on the
WSL2 box. The ruling waits on B.

## Part A — precedence, run 2026-09-05 on the reference host

**Method.** The base image (`ghcr.io/gnrain/nemr-base:0.3.0`, Claude Code
2.1.240) run by hand with `CTRUN … --net-host` (`docs/gpu-env.sh`), with
`ANTHROPIC_BASE_URL` pointed at a tiny HTTP server in rootlesskit's network
namespace that logs every request's `Authorization` header and answers a
minimal Messages response. A fake credential file bound read-only at
`/root/.claude/.credentials.json` exactly as the engine binds the real one;
a fake `CLAUDE_CODE_OAUTH_TOKEN` in the environment. `claude --debug -p
"Reply with the single word OK." --output-format json`, prompt from a file
via stdin. Five cases plus one for `--bare`. The tokens were literal strings
(`FAKE-ENV-TOKEN`, `FAKE-FILE-EXPIRED`, `FAKE-FILE-VALID`); the only real
traffic was Claude Code's own OAuth refresh and profile calls, which carried
those fakes and failed harmlessly.

**Result — the bearer Claude Code sent, per case (two `/v1/messages` requests each):**

| Case | Mounted file | `CLAUDE_CODE_OAUTH_TOKEN` | Sent as `Authorization: Bearer` | Run |
|---|---|---|---|---|
| A | **expired** (`expiresAt` two days ago) | set | **`FAKE-ENV-TOKEN`** | `OK`, exit 0 |
| B | valid (six hours out) | set | **`FAKE-ENV-TOKEN`** | `OK`, exit 0 |
| C | absent | set | **`FAKE-ENV-TOKEN`** | `OK`, exit 0 |
| D | expired | unset | `FAKE-FILE-EXPIRED` | `OK`, exit 0 (control) |
| E | valid | unset | `FAKE-FILE-VALID` | `OK`, exit 0 (control) |
| F | expired | set, **`--bare`** | *nothing sent* | `is_error` — `Not logged in · Please run /login`, exit 1 |

**The env var wins in every file state, including the one the question was
about (A).** In A, B and C the debug log contains no mention of the credential
file, no refresh attempt and no dead-token handling — it never reached the
file path. It identifies the session as an env-var one:

```
[Claude in Chrome] Disabled: OAuth token has no scope accepted by /api/oauth/validate
  (needs user:profile, user:office, or user:ccr_inference; env-var and setup-token
  sessions default to user:inference only)
```

**Case D is Rain's WSL2 loop, in Claude Code's own words.** With the expired
file and no env var, the debug log shows the whole chain:

```
[ERROR] OAuth refresh failed (expected): Request failed with status code 400
[ERROR] OAuth dead-token disk clear: backend write failed
[ERROR] Failed to fetch oauth profile from OAuth token: [REDACTED] Request failed with status code 401
```

then it **sent the expired token anyway** (the capture shows
`FAKE-FILE-EXPIRED`). Refresh fails, the attempt to rewrite the file fails
against the read-only bind — logged at debug level only — and the next call
goes out with the dead token. That is F-130's mechanism, confirmed from the
inside: the read-only mount that correctly forbids writes also forbids the
refresh, and nothing tells the user.

**Case F is the caveat the docs state, confirmed.** `--bare` does not read
`CLAUDE_CODE_OAUTH_TOKEN`; with no API key it makes no request at all. Every
headless path that passes `--bare` (the GPU spike's Arm A did) needs
`ANTHROPIC_API_KEY` or an `apiKeyHelper` instead. Interactive `claude`, which
is what `nemr attach` runs, is unaffected.

**Exposure, measured — what the container can read.** As the Product Owner
asked, stated before the ruling:

```
uid=0
pid1-environ contains the env token: 1 hit(s)      # /proc/1/environ, readable by any process
own environ (any process): 1                        # `env` in a shell
mounted credential readable: yes, mode 600, 236 bytes
mounted credential writable: cannot create /root/.claude/.credentials.json: Read-only file system
```

and on the host: `ctr container info` shows the env var — it lives in the OCI
spec in containerd's database, readable by the user who owns the socket (the
same user who owns `~/.claude/.credentials.json`).

So: **an env-var token is readable by every process in the session, the agent
included — and so is the mounted file today.** The session runs as root and
the file is mode 600 owned by root; `cat /root/.claude/.credentials.json`
works now. The change is not *whether* the agent can read the credential but
*what it reads*: today an 8-hour access token plus a two-week refresh token
that the container cannot use; with option (e), a one-year token. A leak from
inside a session is a longer-lived secret. The scopes are narrower
(`user:inference` only: model requests; no Chrome integration, no Remote
Control, no claude.ai connectors — per the docs and the debug line above).

**Two consequences for the design, if B holds.** (1) *Where the env var is
set.* At create, it is frozen into the container record (the OCI spec is
frozen at create — the F-112 lesson), so rotating the token means recreating
the project, F-12's remedy again. At **attach**, `ctr task exec` takes its own
environment, so the token could be injected per exec, never stored in the
container record, and rotated by re-running attach — that is D-02's own
wording, "injected at attach", taken literally. Part B uses exec-time injection
because it needs no engine change; the ruling should say which the product
does. (2) *The mount.* If the token is the credential, the read-only file
mount becomes unnecessary for Claude Code — but it stays the E-15/Codex path's
concern and AUTH-02's letter; whether to keep binding it is part of the ruling.

**Not established here, deliberately:** that a real setup-token exists in the
form the docs describe; that Anthropic accepts it from inside a session; that
it survives the 8-hour mark; that it survives a login on another machine.
Those are Part B.

## Part B — the real token, on the WSL2 box (Product Owner)

Everything below is by hand; nothing is built. **Never paste the token
anywhere but the shell variable**; record its prefix and length only.

**B1 — mint.** On the WSL2 host, logged in normally:

```bash
claude setup-token
# approve in the browser; the token prints once.
export CLAUDE_CODE_OAUTH_TOKEN='<paste>'
echo "prefix=${CLAUDE_CODE_OAUTH_TOKEN:0:12} length=${#CLAUDE_CODE_OAUTH_TOKEN}"   # record these two, nothing else
```

Record: what the command printed besides the token (any expiry or
"one-year" notice, any scope list), the prefix, the length.

**B2 — a real session, exec-time injection, no engine change.** A throwaway
project, started normally (so the file mount is present as today):

```bash
nemr create tokentest && nemr start tokentest
source ~/src/nemr-engine/docs/gpu-env.sh
SESS=nemr-tokentest
printf '%s\n' "Reply with the single word OK." > ~/tok-p.txt
CTR task exec --exec-id t1 "$SESS" sh -c 'mkdir -p /tmp/p && cat > /tmp/p/p.txt' < ~/tok-p.txt

# the run WITH the token — env at exec time, nothing in the container record:
CTR task exec --exec-id tok1 -t "$SESS" env CLAUDE_CODE_OAUTH_TOKEN="$CLAUDE_CODE_OAUTH_TOKEN" \
  sh -c 'cd /workspace && claude -p --output-format json < /tmp/p/p.txt' | python3 -c 'import json,sys; r=json.load(sys.stdin); print("result:",r.get("result"),"is_error:",r.get("is_error"))'
# the CONTROL, same session, no token — the mounted file:
CTR task exec --exec-id ctl1 -t "$SESS" sh -c 'cd /workspace && claude -p --output-format json < /tmp/p/p.txt' | python3 -c 'import json,sys; r=json.load(sys.stdin); print("result:",r.get("result"),"is_error:",r.get("is_error"))'
nemr status tokentest | grep credential:
```

**Gate B2:** the token run answers `OK` with `is_error: False`. If it fails,
record the verbatim `result` — that is the finding, and (e) is dead.

**B3 — past eight hours.** More than eight hours after the *host's* last
`claude` run (so the mounted file's access token has expired — `nemr status`
says `EXPIRED`), repeat both B2 runs. **Expected:** the token run still
answers `OK`; the control fails with the 401 (F-130's message will have
warned at attach). If the token run fails here too, (e) does not outlive the
file and is dead.

**B4 — a login elsewhere.** Log in on Clawd (`claude`, `/login`). Then repeat
both B2 runs on WSL2. **Expected, and the E-13 question itself:** the token
run still answers `OK`. If it fails with `revoked`, the revocation is
account-level and option (a) — accept and document — is the honest answer;
if only the control fails, E-13 is refresh-token rotation and (e) is the
design. Record both results verbatim, plus what `claude` on the WSL2 *host*
does afterwards (does the host's own login survive the Clawd login?).

**B5 — cleanup, verify first:** `nemr list | grep tokentest` then
`nemr delete tokentest`; `unset CLAUDE_CODE_OAUTH_TOKEN`; the token stays
minted (revoke it at claude.ai if this stays an experiment).

**What to record:** B1's prefix/length and any notice; each run's `result`
and `is_error` verbatim; the timestamps of B2/B3/B4 relative to the host's
last refresh; `nemr status`'s credential line at each step; the Clawd login
time. **Not the token.**

## Part C — option (f): the mounted credential made writable (2026-09-05, reference host)

The Product Owner's ruling superseded Part B: rather than a long-lived token,
make the mounted file writable so Claude Code refreshes it inside the session.
Two things to establish first, both by hand, with the real credential where it
mattered and fakes where it did not.

**C1 — which writable shape persists a write.** Fake credentials, the same
dead-refresh path as Part A's case D, which makes Claude Code *write* the file
(its "dead-token disk clear"):

| Bind | Write persisted? | Inode after | What Claude Code left |
|---|---|---|---|
| single **file**, rw | **yes** | **same** (in place) | `accessToken: ""`, `expiresAt: 0` |
| **directory** at `/root/.claude`, rw | yes | new (rename) | the same, plus `backups/ debug/ projects/ sessions/` created in the host directory |

A rename over a mount point cannot succeed, so through a file bind Claude Code
falls back to an in-place write — which is the better outcome: the host's
path and the session's mount stay the same file.

**C2 — the real refresh, through the file bind.** The host credential copied,
its `expiresAt` moved into the past, the real refresh token left intact; the
copy bound read-write; `claude -p "Reply with the single word OK."` against the
real API:

```
before: access=54ce7b95f18c refresh=563be3f049b1 expiresAt +3 h   mtime=1788580594 ino=525237
result: 'OK' is_error: False api_ms: 2076
after:  access=91ffcc77518b refresh=548f9c825b8a expiresAt +8 h   mtime=1788580605 ino=525237
```

The session refreshed: `OK` in two seconds, the file rewritten in place, the
access token eight hours out, **both tokens rotated**. The rotated pair was
copied back to the host (verified well-formed first), and a host-side
`claude -p` then refreshed again from it — `OK`, both tokens rotated a second
time. Refresh-token rotation on every refresh is therefore observed, not
inferred; and the previous access token kept working after each rotation (the
Claude Code session writing this report was running on it).

**C3 — F-12, reproduced and bounded.** The host's Claude Code wrote by rename
(inode 4872270 → 4872353). On a real project (`testing`), with the task
running: host inode and the task's view agreed at start; after the host file
was replaced by rename, the task still saw the old inode; after `nemr stop`
and `nemr start`, they agreed again. So a running session is pinned to the
file the host had when it started; a host-side refresh strands it on a
rotated-away refresh token; **stop/start is the remedy**, not delete and
recreate. The daemon's user can read the task's view through
`/proc/<pid>/root` and its mount flags through `/proc/<pid>/mountinfo`, so the
state can be detected without an exec and without a write.

**C4 — the enumeration.** `~/.claude` on this host (names and roles only):

```
.credentials.json   the credential — the only thing mounted (600)
history.jsonl       prompt history across every project, 190 KB — never mounted
projects/ sessions/ session state — shadowed in a session by the M8 binds from the volume
settings.json       preferences (F-131's question)
backups/ cache/ debug/ downloads/ file-history/ jobs/ paste-cache/ plugins/
session-env/ shell-snapshots/ daemon* stats-cache.json .last-*   machine-local state and caches
```

and identity (`machineID`, `userID`, `oauthAccount`, plus onboarding flags and
sixty-odd caches) in `~/.claude.json`, outside the directory, never mounted.
The answer to "the whole directory?" is no: one file becomes writable, nothing
else moves, and identity was never in the mount.

**What was built from this** (SPEC 1.102, D-02 revised, F-12 fixed): the bind
is read-write; `start` repairs a pre-(f) record; `status`/`attach` report the
three unrecoverable states — spent refresh token, blanked file, stale mount —
and stay silent on a routine access expiry.

## Part D — killing F-12 with a file bind, and the shape the ruling named (2026-09-05)

The ruling approved "a writable directory holding only the credential."
That shape cannot be built: Claude Code reads the fixed path
`~/.claude/.credentials.json`, the host's file lives in that directory among
the other entries, and a directory that the host's Claude Code also writes
into is `~/.claude` itself (rejected), a nemr-owned directory the host never
writes to (host and session diverge; rotation kills one), or a symlink
(rejected). So the file bind stays, and F-12 has to die another way.

**D1 — the naive re-bind fails, twice, and both failures are kernel facts.**
Inside the task's mount namespace, `mount --bind /proc/self/fd/3 <target>`
fails with "special device does not exist": `/proc` there is the container's
own, so the descriptor cannot be named by path. With the new mount API
(`open_tree(OPEN_TREE_CLONE)` inside rootlesskit's namespaces, `setns` into
the task's mount namespace, `move_mount`), the same sequence that works
within one namespace fails across them with `ENOENT` — because the stale
bind's root dentry is **unlinked** (the host renamed over it) and the kernel
refuses to mount on top of an unlinked dentry.

**D2 — the sequence that works.** Detach the stale bind first
(`umount2(MNT_DETACH)`); the underlying rootfs placeholder is then the mount
point; `move_mount` the detached clone onto it. On a real project with the
task running, after a host-side rename:

```
before: host 4860899  task sees 4872350  (stale)
umount2(stale bind, MNT_DETACH): ok
underlying rootfs placeholder inode: 5908055 size: 0
move_mount(new): ok
after:  host 4860899  task sees 4860899
inside: /home/nemr/.claude/.credentials.json /root/.claude/.credentials.json rw,relatime
```

The running session sees the host's current file, still read-write, with no
stop, no start, no recreate. Joining a user namespace requires a
single-threaded process, so in the engine this runs as `nemrd __rebind`, a
child of the daemon dispatched before its runtime exists.

**D3 — the observability condition and F-12 are one component.** The
daemon's credential watcher (inotify on the host file and its directory)
sees every rewrite: an in-place write is attributed to the running session
(sessions write in place; a rename over a mount point cannot succeed), a
replacement to the host; the result is parsed and named; the record goes to
the daemon log and `nemr status`. And a replacement triggers the re-bind of
every running session — so F-12 dies while the daemon runs, and `attach`
covers a replacement that landed while it did not.

**What is not prevented:** a session can still write junk into the host's
login. It is named, with the session, in the record; the host logs in again.

## The verdict

**Part A settled precedence; Part B is superseded by the ruling for (f); Part C established (f) and it is built.** The original verdict text follows for the record.

### Verdict as written before the ruling

Part A settles precedence: the env var wins, including over an expired file.
Part B decides whether (e) is the design. If B2–B4 hold, D-02 is unchanged in
substance and changed in mechanism — a token generated on the host once,
injected as an environment variable, never on the volume, never in a bundle —
and F-12's inode-pin problem disappears with the file. The ruling also decides
create-time versus attach-time injection and whether the file mount stays.
