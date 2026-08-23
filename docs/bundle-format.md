# Nemr Bundle Format

| Field | Value |
|---|---|
| Document ID | NEMR-BUNDLE-001 |
| Schema version | **1** |
| Status | Draft — implemented by M9/M10 |
| Licence | Open source, part of `nemr-engine` (E-11) |

## Status of this document

This is a **public interface**, not an implementation detail. Per the open-core
ruling (E-11) the format specification is open source and stays that way: the
engine, the wrapper crate, the volume layer, the privileged helper and this
document are the open half; the sync layer, lease service, storage backends,
identity and GUI are the commercial half.

The consequences are binding on anything that touches this format:

1. **It is versioned, and breaking changes are breaking changes.** A reader must
   be able to tell, from the manifest alone and before reading any content,
   whether it can understand a bundle.
2. **No commercial-only escape hatches.** There is no field that only a sync
   layer can populate or interpret, and no bundle that is useless without a paid
   tier. A bundle produced by `nemr export` is completely consumable by
   `nemr import` with no account and no network.
3. **`nemr export` and `nemr import` work standalone**, against a local file.
   Object storage is a transport for bundles, never a prerequisite for them.

The test applied to every design question below: *can someone use the open half
productively without ever paying?* If a choice would have made the answer "no",
it was rejected, and the rejection is noted where it arose.

---

## 1. What a bundle is

A bundle captures **one project's session**, such that importing it on another
machine and attaching resumes the conversation with its history intact.

It is deliberately **not** a disk image. Per D-06 it is file-level: contents are
tarred from inside the *mounted* volume rather than snapshotting the `.img`.
Two reasons, both measured rather than assumed:

- A sparse image tracks the filesystem's high-water mark and never shrinks. A
  project that once wrote 8 GB and deleted it still exports 8 GB.
- ext4 metadata churn destroys delta efficiency between two exports of the same
  project, which is what M13's incremental sync depends on.

It also does **not** carry the base image. The manifest references it by digest
and the destination reconstructs from its own copy — the difference between a
bundle measured in megabytes and one measured in gigabytes.

---

## 2. Physical layout

A bundle is a single file: an uncompressed tar archive whose members are
individually compressed and (from v2) individually encrypted.

```
<name>.nemr
├── manifest.json          # schema version, provenance, member index — ALWAYS FIRST
└── chunks/
    ├── 0000.zst
    ├── 0001.zst
    └── …
```

**The manifest is the first member**, so a reader can stream it and decide
whether it understands the bundle before touching a byte of content. A reader
that finds anything else first must reject the file as corrupt rather than
scanning for the manifest.

### Why the outer archive is uncompressed

This is the load-bearing structural decision, and it exists for M13.

Content is **chunked as plaintext first, then each chunk is compressed
independently**, and from v2 each chunk is encrypted independently. The order
matters and the reverse is a trap: compressing or encrypting a whole stream and
*then* chunking it destroys deduplication, because changing one byte early in
the stream shifts every downstream chunk boundary. This is why restic and borg
chunk before compressing, and it is why a single compressed stream would make
incremental sync a format rewrite rather than a feature.

So the chunk boundary is a real seam in v1, before anything depends on it, even
though v1 ships no deduplication and no encryption. The cost is a slightly worse
compression ratio than one big zstd stream. The benefit is that M13 adds
content-defined chunking and M12 adds per-chunk encryption without either
becoming a new schema version.

### Chunking in v1

v1 uses **fixed 4 MiB boundaries** over the concatenated member stream. This is
simple, deterministic, and gives M13 somewhere to change: swapping to
content-defined chunking (a rolling hash) changes only how boundaries are chosen,
not the layout, the manifest shape, or the reader.

---

## 3. The manifest

```jsonc
{
  "schema_version": 1,
  "engine_version": "0.1.0",
  "created_at": "2026-08-21T00:00:00Z",

  "project": {
    "name": "demo",
    "quota": "2GB",              // the preset the source used
    "content_bytes": 5242880     // uncompressed total, for a pre-flight quota check
  },

  "base_image": {
    "reference": "ghcr.io/gnrain/nemr-base:0.1.0",
    "digest": "sha256:2c4127a5…"   // authoritative; the reference is a hint
  },

  "chunks": [
    { "index": 0, "sha256": "…", "compressed_bytes": 1048576, "plain_bytes": 4194304 }
  ],

  "members": [
    {
      "path": "workspace/notes.md",
      "class": "session-critical",
      "mode": 33188,
      "size": 88,
      "sha256": "…",
      "span": { "chunk": 0, "offset": 0, "length": 88 }
    }
  ],

  "excluded": [
    { "path": "root/.claude/.credentials.json", "reason": "secret" }
  ]
}
```

### `class` — session-critical vs reconstructible

Every member carries a class, taken from the measurements in
[`state-locality.md`](state-locality.md) rather than re-derived:

| Class | Meaning | Examples |
|---|---|---|
| `session-critical` | Losing it loses the session. Must materialise before attach. | conversation transcripts, project memory, session state, workspace files |
| `reconstructible` | Regenerable or re-fetchable; absence costs time, not data. | caches, config backups, housekeeping markers |

This is what makes **lazy materialisation** possible at import: a destination can
restore session-critical members, let the user attach, and fetch the rest in the
background. v1 restores everything eagerly, but the classification is in the
manifest from the start so that becoming lazy is not a format change.

### `excluded` — recording what did not travel, and why

Exclusions are listed explicitly. A bundle that silently omitted content would be
impossible to reason about on the far side, and "why is my MCP config missing"
is exactly the question the field answers.

Recorded reasons: `secret`, `machine-specific`, `cache`, `build-artifact`,
`unrecognised-field`.

---

## 4. Exclusion policy

Exclusion is the compression strategy. Dropping build artifacts is worth roughly
95% on a real project; codec choice is worth roughly 20%. The policy is
therefore part of the format, not a tuning knob.

### Unconditional — never travels

| Path | Reason |
|---|---|
| `/root/.claude/.credentials.json` | **Secret.** D-02: credentials are per-device, injected at attach, never synced. Not configurable, and tested. |

### `.claude.json` — field-level allowlist (F-54)

`/root/.claude.json` mixes portable configuration with machine and account
identity, so it is filtered **per field, not per file**, using an **allowlist**:

- **Travels:** `mcpServers` and project-trust entries.
- **Does not travel:** `machineID`, `userID`, `oauthAccount`, and every cache
  block.
- **Anything unrecognised does not travel, and is logged.**

The allowlist direction is the whole point. A blocklist would silently leak
whatever Anthropic adds in the next Claude Code release — we control neither that
schema nor our notice of it changing — and D-02 would then hold only until the
schema moved. An allowlist defaults new fields to staying put, and the log line
turns schema drift into a visible warning rather than a silent inclusion or a
silent drop.

### Default excluded — overridable

`target/`, `node_modules/`, `.git/objects/` (packs are re-fetchable),
`__pycache__/`, `.venv/`, `/tmp/**`, and the reconstructible items C1 identified
(`/root/.claude/backups/`, `/root/.claude/.last-cleanup`).

---

## 5. Compatibility rules

A reader **must**:

- Reject `schema_version` greater than it supports, naming both versions and
  saying to upgrade (`Error::BundleVersionUnsupported`).
- Reject a bundle whose first member is not `manifest.json`
  (`Error::BundleCorrupt`).
- Verify every chunk digest before use, and every member digest after extraction
  (`Error::ChecksumMismatch`, naming the member).
- Refuse to import when the referenced base image digest is absent locally
  (`Error::BaseImageMissing`) rather than substituting a different image — a
  bundle restored onto the wrong rootfs is a silent-wrong-result defect, the
  class this project has been bitten by repeatedly.
- Check `project.content_bytes` against the destination quota **before**
  extracting (`Error::QuotaMismatch`), so a too-small target fails immediately
  rather than half-way through.

A reader **may** ignore unknown *optional* manifest fields within a supported
schema version; that is what allows additive change without a version bump.

---

## 6. Version history

| Version | Change |
|---|---|
| 1 | Initial. Uncompressed tar; manifest first; fixed 4 MiB chunks, each zstd-compressed; per-chunk and per-member SHA-256; base image by digest; session-critical/reconstructible classification; unconditional credential exclusion; `.claude.json` field allowlist. No encryption, no deduplication — both have their seam reserved. |

### Reserved for later versions

- **v2 — encryption.** Per-chunk AEAD. The chunk boundary already exists, so this
  changes chunk *contents*, not layout.
- **M13 — deduplication.** Content-defined chunking, changing only how boundaries
  are chosen.

Neither requires a reader written against v1 to be restructured, which is the
property the v1 layout was chosen to preserve.

**This is verified, not assumed** — see [`chunking-spike.md`](chunking-spike.md).
A rolling-hash chunker was run over realistic content through the v1 pipeline:
round-trip passed using only fields v1 already records, dedup across an edit near
the start of the stream reached **79.1%** (17 of 21 chunks reused, versus ~0% for
fixed boundaries), chunk identity proved codec-independent, and per-chunk
encryption composed with it. Two v1 choices are what make this work, and neither
should be changed without re-running that spike:

1. `plain_bytes` is recorded **per chunk**, so variable sizing needs no new field.
2. Member spans are **absolute offsets into the concatenated stream**, not
   `(chunk, offset)` pairs — so moving a boundary does not invalidate any span.

The one change M13 may still need is *archive-level*, not manifest-level:
content-addressed chunk naming (`chunks/<sha256>.zst`) if identical chunks are to
be stored once inside a single file. Cross-version dedup more likely lives in
object storage, where a bundle is a manifest plus chunk references — the
commercial side of E-11, requiring no change to the local file at all.
