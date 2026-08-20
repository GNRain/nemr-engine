# State locality of a Claude Code session

**Question (WP C1):** where does a Claude Code session's state actually live —
the container's ephemeral rootfs snapshot, or the portable ext4 volume?

**Answer, in one line:** almost all of it is on the **rootfs snapshot**. The
portable volume held exactly one session-produced file — the project file the
model was asked to write. Conversation history, config, and session state are
all on the layer that does *not* travel. **The product's core premise is unmet
by the current container layout**, and M8 (relocation) is what closes the gap.

This is a finding, not a failure: it is precisely the thing WP C exists to
discover before D-06's bundle format is built on an assumption about it.

---

## Method

Real session, real API, both layers captured **separately** and diffed.

- Fresh project (`nemr create` → `start`), 2 GB volume.
- Baseline manifest of each layer, taken from inside the container. `find / -xdev`
  stays on the overlay rootfs device; `find /workspace -xdev` captures the ext4
  volume; the two never merge because they are different devices. (`-xdev` also
  skips `/proc`, `/sys`, `/dev`, and the credentials bind-mount, all separate
  mounts — itself part of the finding.)
- A **three-turn** Claude Code session against the real Anthropic API, shaped
  like use: a tool-using file write, a continued turn appending to that file,
  and a third `--continue` turn that recalled an earlier fact **from
  conversation history without reading the file** — so the history path is
  genuinely exercised, not just present.
- Re-capture, diff, classify.
- Resumption verified by **history continuity**, not liveness (below).

Raw manifests, diffs and logs: `scratchpad/results/` at capture time; the
load-bearing lines are reproduced here.

Tool-permission note: the container runs as root (`uid 0`), and Claude Code
refuses `--dangerously-skip-permissions` / `bypassPermissions` as root. The
session used `--permission-mode acceptEdits`, which is not root-blocked. This is
worth recording — a future non-interactive/automation path must account for it.

---

## What changed, by layer

**Portable volume (`/workspace`) — 1 path changed:**

```
/workspace/notes.md            88 bytes
```

That is the entire portable footprint of a real session: the project file, and
nothing else.

**Rootfs snapshot — 11 paths changed (all the session state):**

```
/root/.claude.json                                             37579   config + machine identity
/root/.claude/projects/-workspace/<uuid>.jsonl                 24656   CONVERSATION HISTORY
/root/.claude/projects/-workspace/memory/                       —      project memory (dir)
/root/.claude/sessions/                                         —      session state (mode 700)
/root/.claude/backups/.claude.json.backup.<ts>                  50     config backup
/root/.claude/.last-cleanup                                     24     housekeeping
/tmp/cc-socks, /tmp/claude-0                                    —      runtime sockets / temp
```

The conversation transcript — the thing a user means by "my session" — is a
24 KB `.jsonl` under `/root/.claude/projects/`, on the rootfs. It is not on the
volume. An export taken from the volume today would contain `notes.md` and no
history at all.

---

## Classification

For each item: layer, portability class, and whether it is host/machine-specific.
The **session-critical vs reconstructible** column is the one D-06 needs for lazy
materialization, judged now while the evidence is in hand.

| Item | Path | Layer | Class | Host/machine-specific | Secret? |
|---|---|---|---|---|---|
| Conversation history | `/root/.claude/projects/<hash>/*.jsonl` | rootfs | **session-critical** | no | no |
| Project memory | `/root/.claude/projects/<hash>/memory/` | rootfs | **session-critical** | no | no |
| Session state | `/root/.claude/sessions/` | rootfs | **session-critical** | no | no |
| Project file(s) | `/workspace/…` | **volume** | session-critical | no | no |
| Config + identity | `/root/.claude.json` | rootfs | **mixed** (see below) | **yes** (machineID) | no |
| Config backup | `/root/.claude/backups/` | rootfs | reconstructible | no | no |
| Housekeeping | `/root/.claude/.last-cleanup` | rootfs | reconstructible | no | no |
| Caches / GrowthBook / model caches | inside `/root/.claude.json` | rootfs | reconstructible | partly | no |
| Runtime sockets | `/tmp/cc-socks`, `/tmp/claude-0` | rootfs (tmp) | ephemeral | yes | no |
| **Credential** | `/root/.claude/.credentials.json` | **host bind-mount (RO)** | **never travels** | yes | **YES** |
| MCP configuration | `/root/.claude.json` (`mcpServers`) + none present this run | rootfs | session-critical *if used* | no | no |

### Credentials and tokens — the D-02 enumeration

Enumerated by scanning every session-state path for token patterns, then
**verifying each hit** rather than trusting the grep:

- `/root/.claude/.credentials.json` — the **only** secret. Structure could not be
  read as a value (it is the OAuth/token store). It is delivered as a **read-only
  bind mount from the host** (`/dev/sda2 on /root/.claude/.credentials.json …
  ro`), so it is on neither the rootfs snapshot nor the volume, and cannot travel
  in an export of either. This already satisfies D-02 by construction — but note
  it currently sits *inside* `/root/.claude`, the directory M8 relocates, so the
  relocation must not pull it onto the volume (see below).
- The loose scan also flagged the transcript and `.claude.json`. **Both were
  false positives** — the transcript's only match was the literal word "OAuth"
  from an MCP authorization message in the conversation, and the
  unambiguous patterns (`sk-ant-…`, `"access_token":"…"`, `"refresh_token":"…"`)
  matched **nothing** anywhere. This is exactly the "a grep hit is not a secret"
  trap; it is called out so no one re-derives a phantom secret from the same scan.
- `/root/.claude.json` is **not** a secret store, but it holds **machine- and
  account-specific identity**: `machineID`, `userID`, and an `oauthAccount` object
  (accountUuid, emailAddress, organizationUuid, displayName, billing/subscription
  fields). No credential, but this must not travel either — it identifies the
  source device and account.

---

## Resumption — verified by continuity, not liveness

Stop, restart, and ask the model to recall — not "did a process come back up".

```
-- stopping --  stopped project "slocal" (terminated gracefully)
-- starting --  started
-- transcript survived stop/start? --
   /root/.claude/projects/-workspace/<uuid>.jsonl 24656 bytes
-- claude --continue recall --
   fact-alpha: the Eiffel Tower can be 15cm taller in summer
   fact-beta:  honey never spoils
```

Both facts came back, verbatim, from the persisted transcript after a full
stop/start. History continuity holds **because the rootfs snapshot survives
stop/start** (only the task is deleted, not the snapshot). That is also the trap:
resumption *looks* solved, but it is solved by the non-portable layer. The moment
the session moves to a different machine — a volume export/import — the history,
being on the rootfs, is left behind. Which is the whole reason for M8.

---

## Implication for M8 (relocation) and D-06 (bundle)

- **Relocate the session-critical rootfs paths onto the volume**, surgically:
  `/root/.claude/projects/` and `/root/.claude/sessions/`. This is the minimum
  that makes history portable and satisfies the M8 acceptance.
- **Do not relocate wholesale** (e.g. via a blanket `CLAUDE_CONFIG_DIR` override).
  `/root/.claude.json` carries `machineID`/`oauthAccount`, and
  `/root/.claude/.credentials.json` is the credential — a whole-directory move
  drags both onto the exportable layer, violating D-02 and leaking machine
  identity. Surgical bind mounts of just the history subtrees keep secrets and
  identity on the rootfs by construction.
- **Credentials stay a host bind mount**, injected at attach, never on the volume
  — the M8 implementation must make this visible in code (D-02).
- **For D-06's manifest:** `projects/**` and `sessions/**` are session-critical;
  `backups/`, `.last-cleanup`, the cache blocks inside `.claude.json`, and
  `/tmp/*` are reconstructible or ephemeral and excluded; `.credentials.json` and
  the `.claude.json` identity fields are a hard exclusion.

The findings are recorded normatively in SPEC.md Section 11 (pending Product
Owner promotion to a Section 3 subsection, per 4A.5).
