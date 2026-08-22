# Chunking spike — does the v1 manifest absorb content-defined chunking?

**Date:** 2026-08-21 · **Status:** complete, code discarded
**Question:** M13 plans to replace v1's fixed 4 MiB chunk boundaries with
content-defined chunking (a rolling hash). NEMR-BUNDLE-001 v1 claims this is
possible "without a format change." That was a **design claim, not a verified
one** — this spike verifies it before M11 encodes the format's shape in
hardening tests.

**Answer: yes.** The manifest shape absorbs content-defined chunking with **no
new fields and no layout change**. One caveat and one genuinely open question
are recorded at the end.

The spike code was throwaway and has been discarded, as instructed. This document
is the deliverable.

---

## Method

A Rabin-style rolling-hash chunker (48-byte window, 2 KiB min, 64 KiB max,
13-bit boundary mask → ~8 KiB average) run over realistic bundle content: a
~960 KB JSONL transcript shaped like a real Claude Code session (highly
repetitive structure, varying payloads) plus a small project file. The v1
pipeline was then reproduced exactly — build the concatenated plaintext stream,
record member spans, chunk, compress each chunk independently, and reassemble
through the v1 reader's model (concatenate in `index` order, verify each chunk
digest, slice members out by absolute span).

---

## Results

### Q1 — Does the v1 pipeline still round-trip with variable boundaries? **PASS**

```
stream: 958,927 bytes -> 21 chunks (avg 45,663 bytes)
round-trip: PASS — members recovered byte-identical
```

Reassembly used only fields v1 already records: `index`, `sha256` (of
plaintext), `compressed_bytes`, `plain_bytes`, and `span{offset,length}`.
**No new manifest field was needed.**

The two v1 decisions that make this work:

- **`plain_bytes` is recorded per chunk.** Variable-size chunks need no new
  field because v1 never assumed they were uniform — it records each chunk's
  size rather than deriving it from a constant.
- **Member spans are absolute offsets into the concatenated stream**, not
  `(chunk, offset)` pairs. Where chunk boundaries fall is therefore irrelevant
  to member extraction. Had spans been chunk-relative, moving boundaries would
  have invalidated every span and forced a v2.

### Q2 — Does dedup actually work across an edit? **PASS — 79.1%**

Ten bytes inserted 200 bytes into a ~960 KB stream: the worst case for
fixed-size chunking, which shifts every subsequent boundary.

```
fixed 4 MiB (v1 today):  ~0% shared — one inserted byte shifts every boundary
content-defined:         758,850 of 958,937 bytes shared = 79.1%
                         17 of 21 chunks reused
```

This is the M13 prize, and it is real. It also confirms the structural decision
in v1 (chunk plaintext *first*, compress each chunk *independently*) was the
load-bearing one — compressing the whole stream first would have made this 0%
regardless of the chunker.

### Q3 — Is chunk identity still a valid dedup key? **PASS**

`sha256` is taken over the **plaintext** chunk, so identity survives a change of
codec or compression level:

```
sha256(plaintext) stable across zstd level 3 vs 19: PASS
```

A chunk stays the same chunk when compression settings change. Had v1 hashed the
compressed bytes, re-tuning zstd would have invalidated every stored chunk and
dedup would have silently stopped working — a slow, invisible regression.

### Q4 — Does the reserved v2 encryption seam still line up? **PASS**

Each chunk encrypted independently (an XOR stand-in for AEAD — the question is
structural, not cryptographic), then decrypted and reassembled through the same
reader: **PASS**. Because identity remains the *plaintext* digest, encryption
changes chunk **contents**, not layout or manifest shape. v2 and M13 compose
rather than conflict.

### Q5 — What would v1 need to change? **Nothing.**

No new fields, no layout change, no schema bump for the chunking change itself.

---

## Caveat and open question

### Q6 was inconclusive — and the reason matters

I also tested whether duplicate content *within one bundle* (the same library
vendored at two paths) produces duplicate chunks. It reported 0 duplicates
across 5 chunks, which is **not** a meaningful result: my spike chunker did not
re-synchronise across the copy boundary, which reflects toy-quality rolling-hash
code rather than a property of content-defined chunking. A real chunker
(FastCDC, or borg's buzhash) re-syncs within roughly one average chunk.

Recording this rather than quietly dropping it, because the number looked like a
finding and was not one.

### The one real change M13 will need — and it is not the manifest

For dedup to *save bytes*, identical chunks must be **stored once**. v1 names
archive members by index (`chunks/0000.zst`), so two chunks with the same digest
occupy two members. Storing once requires **content-addressed naming**
(`chunks/<sha256>.zst`).

That is an archive-layout change, not a manifest change — the manifest already
carries the digest that would name them. It is also probably not where M13's
value lands: cross-*version* dedup happens in **object storage** (upload each
chunk once, keyed by digest; a bundle becomes a manifest plus references), which
is the commercial sync layer's concern under E-11 and does not require the local
`.nemr` file to change at all.

**The useful reframing this spike produced:** the *manifest* is the durable
public interface; the tar is one packaging of it. Keeping the manifest stable is
what matters, and the spike confirms it is.

---

## Conclusion

| Question | Answer |
|---|---|
| Does the manifest shape absorb content-defined chunking? | **Yes — no new fields** |
| Does dedup work across an early edit? | **Yes — 79.1%, 17/21 chunks reused** |
| Is chunk identity codec-independent? | **Yes — plaintext hashing was correct** |
| Does the v2 encryption seam still line up? | **Yes — contents change, not layout** |
| Is a v1 format change needed *now*? | **No** |
| Anything M13 will need beyond the manifest? | Content-addressed chunk *naming*, if intra-file dedup is wanted; likely unnecessary since cross-version dedup lives in object storage |

The v1 format proceeds to M11 unchanged. The "M13 without a rewrite" claim in
`docs/bundle-format.md` is now verified rather than asserted, and that document
should say so.
