# Verifying SPEC 1.151 — what running it found

**Date:** 2026-09-11
**Branch:** `one-voice-and-one-file` (PR #94, open against `main`)
**Asked for:** *"Verify the last features I asked from you."*

Verification here means the features were **run**, on this machine, the way the
Product Owner runs them — not re-read against the report that described them.
Three defects came out of that, and a fourth finding that is not a code defect.
All four are below, with the evidence, the fix, and the assertion that now
holds each one down.

---

## 0. The first command, and why it is the whole report

The first thing run was the thing he would run:

```
$ env -i PATH=/usr/bin:/bin:$HOME/.local/bin HOME=$HOME nemr server status
XX No sync server is running.

  running:    no
              nothing is listening
  address:    127.0.0.1:8080
  storage:    not configured  (unconfigured: no storage backend is configured; set exactly one: NEMR_BUNDLE_DIR=<existing directory>, or NEMR_S3_PROVIDER, NEMR_S3_BUCKET, NEMR_S3_ENDPOINT, NEMR_S3_ACCESS_KEY_ID and NEMR_S3_SECRET_ACCESS_KEY for an object store)
  database:   postgres://nemr:***@127.0.0.1:5433/nemr  (answers)
  pepper:     configured
  settings:   /home/nemr/.config/nemr/sync.env  (keys: DATABASE_URL,NEMR_SERVER_ADDR,NEMR_AUTH_PEPPER)
```

Everything 1.151 claimed is visible in that output: the result line first, the
file read with nothing exported, the database password masked, the settings
file named with its keys. And it still does not work, because the storage
credentials are not in that file — they are in `~/.config/nemr/r2.env`, which is
the second file he has been sourcing by hand every session. That is the
complaint 1.151 was written from.

So the feature was correct and the machine it was built for was still broken.
Everything below follows from that one line.

---

## 1. `configure` refused the only file it needed to fix

**What it did.** `nemr server configure` refused to run when `sync.env` exists:

> `/home/nemr/.config/nemr/sync.env already exists.`
> **Because:** this command writes that file, and will not write over settings you may be running a server on
> **Fix:** edit it, or move it aside and run this again: `mv … …/sync.env.old`

**Why that is wrong.** "It exists" and "it is complete" are different things,
and the common case is the second. `install_server.sh` writes `DATABASE_URL`,
`NEMR_SERVER_ADDR` and `NEMR_AUTH_PEPPER` into that file; the storage
credentials went into a second file because, before 1.151, nothing said they
could go into the first. Every machine set up that way — including the only
machine in existence that has been set up that way — hits the refusal. The
remedy it offered (`mv sync.env sync.env.old`) throws away a working pepper and
a working database line to re-type both.

**What it does now.** It asks the server's own preflight what is missing and
adds only that:

```
$ nemr server configure
nemr server configure
⚠ …/sync.env exists, but it is missing the storage backend.
              Nothing already in it will be changed — this only adds what is absent.

Where do the sessions get stored?
    1  a folder on this machine, or a mounted network drive
    2  object storage on your own network (MinIO, Garage, anything S3)
    3  cloud object storage (Cloudflare R2, Backblaze B2, Amazon S3)
  Choice [1]: 3
    r2  Cloudflare    b2  Backblaze    s3  Amazon
  Provider [r2]: …
  Endpoint URL: …
  Bucket [nemr]: …
  Access key ID: …
  Secret access key (not shown):

Checking, before writing anything:
  database:   answers
  storage:    answers

Adding to …/sync.env:
  NEMR_S3_PROVIDER=r2
  NEMR_S3_BUCKET=nemr-dev
  NEMR_S3_ENDPOINT=<not shown>
  NEMR_S3_ACCESS_KEY_ID=<not shown>
  NEMR_S3_SECRET_ACCESS_KEY=<not shown>

✓ …/sync.env updated.
  Start it:   nemr server start
```

Three rules hold it:

- **It appends. It never rewrites.** Existing lines are not parsed, reordered or
  re-emitted. The file may hold a pepper this command has no business touching.
- **It checks before it writes.** The candidate is the real file plus the new
  lines, written 0600 to a temporary file beside the destination and handed to
  the server's preflight. What answered is what gets renamed into place, so the
  file on disk is the file that was verified.
- **A complete file is still refused**, in different words, so the two cases are
  told apart rather than merged.

**Ran, with the real bucket** (`docs/reports/` holds no credential; the run
used `~/.config/nemr/r2.env`): a file holding only the three
`install_server.sh` keys was completed to five `NEMR_S3_*` keys, and its first
three lines came back byte-identical.

**The cost.** `configure` is no longer a single-shot writer; it has two paths,
and the second one appends to a file it did not write. The mitigation is that
it never reads existing settings back out and never re-emits them — the only
thing it can do to an existing line is leave it alone.

---

## 2. `configure` printed the R2 account id

**What it did.** The confirmation redacted the pepper, both keys and the
database password — and showed the endpoint:

```
  NEMR_S3_ENDPOINT=https://<the account id>.r2.cloudflarestorage.com
```

An R2 endpoint carries the Cloudflare account id. `nemr server status` has
withheld it since E-20 and the acceptance has asserted that since 1.151
(*"and no endpoint (it carries the account id)"*). `configure` was written
later and did not inherit the rule.

Against the brief — *"Nothing may print a secret: the pepper and the storage
credentials must never appear in any output, including status, configure's
confirmation, and any refusal naming the file"* — this is a straight miss.

**Fixed.** `NEMR_S3_ENDPOINT` joins the redacted set, for every provider rather
than only for `r2`. A self-hosted MinIO endpoint is not a secret, but a rule
that has to decide per provider is a rule that leaks the day the decision is
wrong. The wording is now accurate about why each is hidden — the pepper reads
`<generated, not shown>` because the machine made it; the rest read
`<not shown>` because the operator typed them.

---

## 3. The refusal he actually meets named no command

**What it did.** One line, 330 columns, five variable names, no command, and no
mention of the file the variables are supposed to live in — see §0. When the
same text arrived through `--check` it carried a literal `\n`, because the
report is one line per fact so it can be parsed and nothing put the line breaks
back.

**Against the brief.** *"Failures name the cause and the fix as a command to
run."* It named the cause five times over and the fix not at all.

**What it does now:**

```
  storage:    not configured
              no storage backend is configured; it needs exactly one:
              NEMR_BUNDLE_DIR=<existing directory>, or NEMR_S3_PROVIDER,
              NEMR_S3_BUCKET, NEMR_S3_ENDPOINT, NEMR_S3_ACCESS_KEY_ID and
              NEMR_S3_SECRET_ACCESS_KEY for an object store.
  Fix:        nemr server configure
```

- The value stays short and the reason goes underneath — which is what
  `running: no` / `nothing is listening` already did two lines above it, so this
  is the existing voice applied rather than a new one.
- `Voice::notes` wraps at **80 columns, fixed**, and does not break a word that
  is longer than the column. The fork was between a fixed width and asking the
  terminal for its own: `nemr-style` has no dependencies on purpose, and asking
  costs either `libc` or a terminal crate in a crate both halves link. A wrong
  guess at a 200-column terminal costs a short line; a dependency there costs
  the seam. Fixed width, and said so in the code.
- The remedy is promoted to a `Fix:` line by `nemr_style::remedy` — the rule
  `error_block` already used to promote `…: nemr …` lines, now extracted so
  there is exactly one of it. A command split across two wrapped lines cannot be
  copied, which is the only thing a command in an error message is for.
- The escaped newlines from the one-line report are put back before display.
- `unconfigured:` is dropped when the value already reads `not configured` —
  it was the same word twice.

---

## 4. Not a code defect: the installed binary was not the built one

`~/.local/bin/nemr-sync` was four hours older than `target/release/nemr-sync`
(`ad220f8ec8b9` installed, `05cba87f40a4` built). It predated the
endpoint-scheme guard 1.151 added — so the binary being verified was not the
binary that had been tested, and a self-hoster typing `minio.home.lan:9000`
would still have hit the panic on this host.

All three are reinstalled and hash-matched against the build. The previous
binaries are kept outside the repo for this session in case a comparison is
wanted.

Worth noting for its own sake: `install_server.sh` builds `-p nemr-sync -p
nemr-cloud` and installs only `nemr-sync`. That is not touched here.

---

## 3b. And the page that first run prints never mentioned `configure`

Found by fixing §3: `nemr server start`, with nothing configured, prints a full
page of settings and told the reader to make a pepper by hand —

```
  # The auth pepper. THERE IS NO DEFAULT AND NOTHING GENERATES ONE FOR YOU:
  …
  #   umask 077; mkdir -p "$(dirname …)"
  #   printf 'NEMR_AUTH_PEPPER=%s\n' "$(head -c 32 /dev/urandom | base64 -w0)" >> …
```

— without once naming the command that had just been built to do exactly that.
A page of instructions that does not mention the one-line alternative wastes
the reader's afternoon. It now opens with it:

```
The server reads one file — …/sync.env — mode 0600.

To be asked for what goes in it and have it written for you: nemr server configure

To write it yourself, the settings are below. …
```

The pepper paragraph now reads **THE SERVER NEVER GENERATES ONE FOR YOU**
instead of **NOTHING GENERATES ONE FOR YOU**, because the second stopped being
true the day `configure` shipped. E-19's rule is about the server — it never
invents a pepper, and `start` still refuses without one — and that is unchanged.
The hand-rolled recipe stays, for anyone who wants it.

---

## Assertions

`scripts/server_acceptance.sh`, both arms run on this machine:

| Arm | Before | After |
|---|---|---|
| directory storage | 63 → 68 (1.151) | **72** |
| object storage (real bucket) | 68 → 78 (1.151) | **82** |

Nine new assertions:

1. the value stays short and the reason goes underneath
2. no escaped newline reaches the reader
3. and the remedy is one whole command, on its own line
4. it names what is missing instead of refusing the file
5. lines were added and every line already there is byte-identical
6. and the storage it was missing is now in the file
7. the completed file is still 0600
8. and the completed file starts a server with nothing exported
9. and the template names the command that writes the file for you (replacing
   *"it says plainly that no pepper is generated for you"*, reworded below)

plus, in the object-store arm only: it completes a file with a real object
store; the secret access key / the access key id / the endpoint never appear in
its confirmation; and the file it wrote holds both, so the redaction is not an
empty file.

### Neuters, each red before it was green

| Neuter | Red |
|---|---|
| `add_missing` removed — any existing file refused, as before | 4, 5, 6, 7, 8 |
| `NEMR_S3_ENDPOINT` dropped from the redacted set | *nor the endpoint, which carries the account id* |
| `say_state` restored to the single-line value | 1, 3 |
| `unescape` removed from `describe_state` | 2, 3 |
| the template's pointer to `configure` removed | 9 |

**Two neuters came back green the first time, and the assertions were wrong,
not the guard.** "Every line already there is byte-identical" and "still 0600"
both hold for a file nothing touched — which is exactly the state the neuter
produces. Each now asserts the file **grew** as well, and both go red.

### Parsed output changed deliberately

- *"a second run refuses rather than overwriting, and names the file"* now
  matches `already has everything a server needs` instead of `already exists`.
  The old wording was also what an incomplete file was told, so the assertion
  could not tell the two cases apart.
- *"it says plainly that no pepper is generated for you (E-19)"* now matches
  `THE SERVER NEVER GENERATES ONE FOR YOU` instead of `NOTHING GENERATES ONE FOR
  YOU`. The old text stopped being true when `configure` shipped; the rule it
  was guarding — the server invents no pepper — is unchanged and still asserted.

Those are the only two parser changes.

`nemr list` and `nemr status` are untouched by this revision.

### Unit tests

`nemr-style` 10 (was 9), `nemr-cloud` 28 (was 26). New:
`a_long_note_wraps_and_every_line_lines_up`,
`a_file_missing_only_its_storage_is_completed_not_refused`,
`what_is_added_carries_its_own_blank_line`, and the endpoint added to
`the_confirmation_shows_no_secret`.

`cargo fmt --all --check` clean. `scripts/check_seam.sh` green — `nemr-style`
is an OPEN crate and the engine still stands alone.

---

## What is left for the Product Owner to do, once

One command, on this machine:

```
nemr server configure
```

It will say `sync.env exists, but it is missing the storage backend`, ask the
five storage questions, check them against the real bucket, and append them.
The pepper and the database line already in that file are not touched. After
that, `~/.config/nemr/r2.env` is no longer read by anything and can be deleted.

That was not done here: it writes live credentials into his running
configuration, and the decision of when to do that is his. Everything needed to
make it safe has been verified — the same operation was run end to end against
the real bucket, in a sandbox, with his real credentials, and the pre-existing
lines came back byte-identical.
