# One file for the server's settings, one command to write it, and one voice for every command

2026-09-11. Three things the Product Owner asked for in one message: stop
needing exported variables, add an interactive `nemr server configure`, and
make every command in both binaries sound like the same program. This is what
each turned into, what it cost, and where there was a real fork.

---

## 1. Nothing has to be exported any more

The complaint, verbatim:

> I still set variables by hand every session:
> `set -a; source ~/.config/nemr/r2.env; set +a`
> `export DATABASE_URL=postgres://nemr:nemr@127.0.0.1:5433/nemr`

**The first finding is that the server never needed the second file.** E-20 put
`NEMR_S3_*` in `sync.env` on purpose, and the lookup the storage layer receives
is the file-aware one. Measured, with everything in `sync.env` and `env -i`:

```
$ env -i PATH=… HOME=… NEMR_SYNC_ENV_FILE=…/sync.env nemr server status
✓ The sync server is up.
  storage:    r2:<bucket>  (answers)
  database:   postgres://nemr:***@127.0.0.1:5433/nemr  (answers)
```

So nothing had to be built for the server itself. What had to change is
everything around it: one script said the opposite, nothing tested it, and the
scripts and tests still demanded exports of their own.

### What now reads the file

| Reader | Before | Now |
|---|---|---|
| `nemr-sync` (the server) | already read `sync.env` for every setting | unchanged, and now asserted |
| `scripts/server_acceptance.sh` | `: "${DATABASE_URL:?required}"` | reads `sync.env`, refuses naming the file and the key |
| `scripts/sync_acceptance.sh` | a hard-coded development default | reads `sync.env`, then that default |
| `crates/nemr-sync/tests`, `crates/nemr-cloud/tests`, the UI surface test | `std::env::var("DATABASE_URL").expect(…)` | `settings::setting("DATABASE_URL")` — environment, then file, then a refusal naming both |
| `NEMR_MAX_BUNDLE_BYTES`, `NEMR_DB_MAX_CONN` | `std::env::var` only | the file too |
| `scripts/install_server.sh` | *"if you want S3 or R2 … set `NEMR_S3_*` in the environment you start the server in"* | that text was **wrong**; it now points at `nemr server configure` |

The mechanism is two small things, one per language, with the same rules:
`settings::setting(key)` in Rust and `scripts/lib/settings.sh` in shell. Both
take the environment first, then the file; both refuse a file wider than 0600;
both name the file **and** the key when something is missing. Neither ever
prints a value.

### What genuinely cannot come from sync.env, and why

`sync.env` is the **server's** file. These are not server settings:

| Variable | Whose | Why it cannot live there |
|---|---|---|
| `NEMR_SERVER_URL` | the client | which server to talk to is a property of your machine, not of a server; it is already remembered in the client's own state after a login |
| `NEMR_CLOUD_EMAIL`, `NEMR_CLOUD_PASSWORD` | the client | an identity, prompted for; a password in a file is the thing E-16 exists to avoid |
| `NEMR_CLOUD_HOLDER` | the client | the lease holder's name — this machine's identity |
| `NEMR_SYNC_BIN` | `nemr server` | it names the binary that would read the file, so reading it from the file is circular |
| `NEMR_BIN`, `NEMR_INSTALL_DIR`, `BROWSER` | the host | where programs are, not what a server needs |
| `DATABASE_URL` when there is no file yet | — | `nemr server configure` writes it; before that there is nothing to read |

## 2. `nemr server configure`

Four questions, sensible defaults, and it checks the answers before it writes
anything. It generates the pepper itself and never asks anyone to invent one.

```
nemr server configure
  Four questions, then it checks the answers and writes the file.
  Press enter to take the default in [brackets].

  Port to listen on [8080]:
  Where do the sessions get stored?
    1  a folder on this machine, or a mounted network drive
    2  object storage on your own network (MinIO, Garage, anything S3)
    3  cloud object storage (Cloudflare R2, Backblaze B2, Amazon S3)
  Choice [1]:
  Postgres connection [postgres://nemr:nemr@127.0.0.1:5433/nemr]:

Checking, before writing anything:
  database:   answers
  storage:    answers

Writing /home/you/.config/nemr/sync.env:
  NEMR_SERVER_ADDR=127.0.0.1:8080
  DATABASE_URL=postgres://nemr:***@127.0.0.1:5433/nemr
  NEMR_BUNDLE_DIR=/home/you/.local/share/nemr/bundles
  NEMR_AUTH_PEPPER=<generated, not shown>

✓ /home/you/.config/nemr/sync.env written.
  mode:       0600 — it holds the pepper and the storage credential
  Start it:   nemr server start
```

It refuses rather than surprising you: no terminal (it asks questions), a file
that already exists (it names it and says how to move it aside), and a database
or store that does not answer (it writes nothing at all). The confirmation
redacts the pepper, both S3 keys and the database password — asserted by
comparing the generated pepper against the command's own output.

### Self-hosted object storage: verified, not assumed

The brief said to check whether it already works before building anything. It
does. A real MinIO was started in a throwaway container and the repository's own
storage round-trip and the server's egress-free preflight were run against it:
both pass with `NEMR_S3_PROVIDER=s3` and a custom endpoint. Nothing was built.
The backend already uses path-style addressing for every provider, and already
allows plain `http://` when the endpoint says so — which is exactly what a box
on your own network needs.

Two things came out of that check:

- **`NEMR_S3_PROVIDER=minio` is refused**, and should be: `s3` is the generic
  name, and a second name for identical configuration is the vendor branching
  E-11 forbids. Option 2 in `configure` writes `s3` for you.
- **An endpoint with no scheme used to PANIC** — `NEMR_S3_ENDPOINT=minio.home.lan:9000`
  exited 101 inside a URL parser, naming a crate the reader has never heard of.
  Somebody self-hosting is the person most likely to type it that way. It now
  refuses by name.

## 3. One voice

Before this, **neither binary emitted a single colour byte of its own** — the
only styling anywhere came from clap's help renderer. Output was a mix of
`created project "x"`, `started project "x" (supervisor pid 41)` and
`nemr server: stopping pid 41`, with the important word in a different place
every time.

Now there is one crate, `nemr-style`, that both halves depend on. It is open,
it has no dependencies, and it carries the four rules:

| Rule | How it is enforced |
|---|---|
| Result first | `Voice::done/warned/failed` — every command's first line |
| Colour with meaning | `Tone::{Good, Pending, Bad, Dim, Plain}`; the mapping to green/amber/red/dim lives in one place |
| Failures name the cause and the fix | `error_block` renders every `anyhow` error the installer's way, promoting a `…: nemr …` line to **Fix** |
| Values carry the colour, labels do not | `Voice::field(label, value, tone)` — the label is dim, the value is toned |

Colour is decided once per stream, by the installer's rule: a terminal, no
`NO_COLOR`, and a `TERM` that is not `dumb`. With colour off the marks become
`OK` / `XX` / `!!`, because a tick that renders as a box is worse than two
letters that do not.

### The Product Owner's own example

> In `nemr server status`, "running: no" reads red and "(answers)" green —
> today it is one colour and I read every line to find the one that matters.

```
✗ No sync server is running.

  running:    no                     ← red
              nothing is listening
  address:    127.0.0.1:8080
  storage:    r2:bucket  (answers)   ← green
  database:   postgres://nemr:***@127.0.0.1:5433/nemr  (answers)   ← green
  pepper:     configured             ← green
  settings:   /home/you/.config/nemr/sync.env  (keys: …)
```

### Long-running commands

`push` and `pull` used to print a line per step and leave the lot behind. The
steps now redraw in place and are erased when the command ends, the way the
installer does it — what a long command leaves behind should be its result:

```
✓ Pushed proj.
  uploaded:   31 MiB encrypted, from 107 MiB
  lease:      released — another machine can take this session
```

`create`, `add`, `start` and the image pull already reported through the
daemon's own step lines; those keep their existing progress and gained the
result line.

### Every command, before and after

### `nemr list`

**Before**
```
NAME               STATUS    USED               QUOTA    VOLUME
a7a                stopped   unmounted          500MB    /home/nemr/.local/share/nemr/mounts/a7a
authcheck          stopped   unmounted          500MB    /home/nemr/.local/share/nemr/mounts/authcheck
sizetest           stopped   5.8MiB (1%)        500MB    /home/nemr/.local/share/nemr/mounts/sizetest
test-ui-vm         stopped   unmounted          500MB    /home/nemr/.local/share/nemr/mounts/test-ui-vm

1 volume image(s) belong to no project and are using disk:
  enospc-199675-47
Reclaim them with: nemr reconcile
```

**After**
```
NAME               STATUS    USED               QUOTA    VOLUME
a7a                stopped   unmounted          500MB    /home/nemr/.local/share/nemr/mounts/a7a
authcheck          stopped   unmounted          500MB    /home/nemr/.local/share/nemr/mounts/authcheck
sizetest           stopped   5.8MiB (1%)        500MB    /home/nemr/.local/share/nemr/mounts/sizetest
test-ui-vm         stopped   unmounted          500MB    /home/nemr/.local/share/nemr/mounts/test-ui-vm

⚠ 1 volume image(s) belong to no project and are using disk:
  enospc-199675-47
  Reclaim them: nemr reconcile
```

### `nemr status <project>`

**Before**
```
a7a
  agent:        Claude Code
  state:        stopped
  container:    nemr-a7a
  volume:       /home/nemr/.local/share/nemr/mounts/a7a
  usage:        unmounted (quota 500MB)
  image file:   /home/nemr/.local/share/nemr/volumes/a7a.img (present)
  loop device:  none
  mount check:  n/a (not mounted)
```

**After**
```
✓ sizetest is stopped.
  agent:        Claude Code
  state:        stopped
  container:    nemr-sizetest
  volume:       /home/nemr/.local/share/nemr/mounts/sizetest
  usage:        5.8MiB of 500MB (1%)
  image file:   /home/nemr/.local/share/nemr/volumes/sizetest.img (present)
  loop device:  /dev/loop13
  mount check:  ok (backed by this project's image)
```

### `nemr status <unknown>`

**Before**
```
Error: no project named "no-such-proj"
Create it first: nemr create no-such-proj --size 2GB
```

**After**
```
Error: no project named "no-such-project"
Create it first: nemr create no-such-project --size 2GB
```

### `nemr start <unknown>`

**Before**
```
Error: no project named "no-such-proj".
Create it first: nemr create no-such-proj --size 2GB
```

**After**
```
Error: no project named "no-such-project".
Create it first: nemr create no-such-project --size 2GB
```

### `nemr stop <unknown>`

**Before**
```
Error: no project named "no-such-proj".
Create it first: nemr create no-such-proj --size 2GB
```

**After**
```
Error: no project named "no-such-project".
Create it first: nemr create no-such-project --size 2GB
```

### `nemr delete <unknown>`

**Before**
```
Error: no project named "no-such-proj".
Create it first: nemr create no-such-proj --size 2GB
```

**After**
```
Error: no project named "no-such-project".
Create it first: nemr create no-such-project --size 2GB
```

### `nemr add <missing dir>`

**Before**
```
Error: the directory "/no/such/directory" cannot be read: No such file or directory (os error 2)
```

**After**
```
Error: the directory "/no/such/dir" cannot be read: No such file or directory (os error 2)
```

### `nemr sessions (logged out)`

**Before**
```
Error: not logged in (no /home/nemr/.local/state/nemr/cloud/account.json). Run: nemr login

Caused by:
    No such file or directory (os error 2)
```

**After**
```
Error: not logged in (no /home/nemr/.local/state/nemr/cloud/account.json). Run: nemr login

Caused by:
    No such file or directory (os error 2)
```

### `nemr push <unknown>`

**Before**
```
Error: not logged in (no /home/nemr/.local/state/nemr/cloud/account.json). Run: nemr login

Caused by:
    No such file or directory (os error 2)
```

**After**
```
Error: not logged in (no /home/nemr/.local/state/nemr/cloud/account.json). Run: nemr login

Caused by:
    No such file or directory (os error 2)
```

### `nemr release <unknown>`

**Before**
```
Error: not logged in (no /home/nemr/.local/state/nemr/cloud/account.json). Run: nemr login

Caused by:
    No such file or directory (os error 2)
```

**After**
```
Error: not logged in (no /home/nemr/.local/state/nemr/cloud/account.json). Run: nemr login

Caused by:
    No such file or directory (os error 2)
```

### `nemr server status`

**Before**
```
nemr server
  running:   yes
             pid 264695, started by this command
  address:   127.0.0.1:8080
  /health:   answers
  storage:   not configured  (unconfigured: no storage backend is configured; set exactly one: NEMR_BUNDLE_DIR=<existing directory>, or NEMR_S3_PROVIDER, NEMR_S3_BUCKET, NEMR_S3_ENDPOINT, NEMR_S3_ACCESS_KEY_ID and NEMR_S3_SECRET_ACCESS_KEY for an object store)
  database:  postgres://nemr:***@127.0.0.1:5433/nemr  (answers)
  pepper:    configured
  settings:  /home/nemr/.config/nemr/sync.env  (keys: DATABASE_URL,NEMR_SERVER_ADDR,NEMR_AUTH_PEPPER)
```

**After**
```
✗ No sync server is running.

  running:    no
              nothing is listening
  address:    127.0.0.1:8080
  storage:    not configured  (unconfigured: no storage backend is configured; set exactly one: NEMR_BUNDLE_DIR=<existing directory>, or NEMR_S3_PROVIDER, NEMR_S3_BUCKET, NEMR_S3_ENDPOINT, NEMR_S3_ACCESS_KEY_ID and NEMR_S3_SECRET_ACCESS_KEY for an object store)
  database:   postgres://nemr:***@127.0.0.1:5433/nemr  (answers)
  pepper:     configured
  settings:   /home/nemr/.config/nemr/sync.env  (keys: DATABASE_URL,NEMR_SERVER_ADDR,NEMR_AUTH_PEPPER)
```

### `nemr server stop (nothing running)`

**Before**
```
nemr server: nothing to stop — no server started by this command is running.
             (`nemr server status` also looks at the address itself.)
```

**After**
```
✓ Nothing to stop.
              No server started by this command is running.
              `nemr server status` also looks at the address itself.
```


## 4. What was deliberately changed that something parsed

The Product Owner's constraint: *"`nemr list` and `nemr status` are parsed by
the acceptance scripts — keep them green or update them deliberately and say
which."*

**`nemr list` is byte-identical when piped.** Its words and columns are
untouched; only the STATUS value is painted, and only on a terminal. A coloured
cell is padded by its VISIBLE width — escape bytes are invisible and `{:<9}`
would count them, walking the column right — which has its own test.

**`nemr status` keeps its label column and every word**; it gained a result
line above them and colour on four values. The one script that pipes it greps
for `credential:` and `last rewrite`, which are untouched.

Four assertions elsewhere were updated deliberately, and here they are:

| Where | Was | Why it changed |
|---|---|---|
| `crates/nemr-cloud/tests/cli.rs` | `out.contains("no sessions")` | the result line is a sentence: `No sessions, here or on the server.` — now matched case-insensitively |
| `crates/nemr-cloud/tests/cli.rs` | `out.contains("pushed")` | `Pushed proj.` — same, case-insensitive |
| `tests/regression.rs` (F-15) | `out.contains("added")` | `Added /path as name.` — same |
| `scripts/sync_acceptance.sh` | breaks its read loop on `logged in as` | register's last line is now `Account created, and the recovery code confirmed.` |
| `docs/ui-acceptance.sh` | `grep '^nemr server: starting'` | the banner is now `OK Starting the sync server.` |
| `scripts/server_acceptance.sh` | `grep "address:   $ADDR"` | the shared field layout pads to a common column; the assertion is spacing-tolerant now |

Nothing else that reads output changed, and every acceptance was re-run.

## 5. Where there was a real fork

**One crate or two copies of the rules.** The engine must not depend on the
commercial half (E-11), and the commercial client deliberately does not link
the engine. So a shared voice needed a third, open crate that both may depend
on. The alternative was copying forty lines into each binary, which is how two
voices become three. `nemr-style` has no dependencies and no policy — it turns a
value and a tone into a string — and `check_seam.sh` still passes.

**Colour on `list`, or nothing on `list`.** Painting a table column risks the
three scripts that match `^<name> `. Keeping the words and columns byte-identical
and painting only the status value, only on a terminal, means a pipe sees
exactly what it saw before. The cost is the padding helper and its test; the
alternative — leaving `list` plain — would have made the one command people run
most the one command that does not speak the language.

**Redaction in `configure`'s confirmation, or no confirmation.** Showing the
file before writing it is the point of the step, and the file is mostly
secrets. Showing the shape with the values replaced keeps both.

## 6. Asserted

`scripts/server_acceptance.sh`, **63 assertions in directory mode and 68 with
an object store**, the count asserted, both run here — the object-store arm
against the real bucket with the credentials read from `sync.env` and **nothing
exported**.

New, across three sections:

- with nothing exported at all, a server reads everything from the file, and
  says which file it read
- `configure` refuses without a terminal, writes 0600, says in the file's own
  header that it is a secrets file, generates the pepper, writes exactly one
  backend, ends by naming `nemr server start`, and refuses a second run
- the pepper it generated never appears in its own output
- the file it wrote answers for both the database and the store, with nothing
  exported
- piped, the first line of every command is the result and there is not one
  escape byte; on a terminal it is coloured, and the label is dim while the
  value carries the colour; `NO_COLOR` and `TERM=dumb` turn it off

### Proved red first

| Neuter | The guard's answer |
|---|---|
| a setting stops falling back to the file | RED — the unit guard fails, naming the key and the file |
| `configure` stops refusing an existing file | RED — a second run overwrites |
| `configure` stops redacting its confirmation | RED — the generated pepper appears in its own output |
| the voice stops honouring `NO_COLOR` | RED — colour on a terminal that asked for none |
| `server status` stops leading with a result line | RED — the first line is no longer a result |

## 7. What is left that still needs an export

Nothing, for a server. For a client, `NEMR_SERVER_URL` is the only one worth
knowing about, and it is already remembered after your first login — the
environment is the override, not the source. The full list, with the reason
each one is not a server setting, is in §1.
