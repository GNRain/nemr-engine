# `nemr server start` — one command instead of a screen of environment variables

2026-09-11. The Product Owner:

> Running the sync server takes too many commands. I set environment variables
> by hand every time — the pepper, the bundle directory or the R2 variables, the
> database URL, the listen address — and I get it wrong regularly. Give me one
> command.

Built as `nemr server start|stop|status` on the commercial half. This is what it
does, what it refuses, what it deliberately does not do, and the three defects
found on the way.

---

## 1. Where it lives, and who it is for

On **nemr-cloud**, the commercial half, reached through the open CLI's
external-subcommand fallback: `nemr server` execs `nemr-server`, which is the
same one binary wearing another name. The open engine learns nothing (E-11).

Its help says who it is for, in the first sentence:

```
Run the sync server: one command instead of a screen of environment variables.
SELF-HOSTING AND DEVELOPMENT ONLY — the hosted product does not need this, and
a normal user never starts a server.

Settings come from sync.env and the environment (E-19). It does not install or
start Postgres: it checks that one answers and refuses, naming the connection
it tried.
```

## 2. What it does not do: Postgres

It checks and refuses. It does not install, start, create or migrate a database:

```
nemr server: Postgres did not answer.

  tried:   postgres://nemr:***@127.0.0.1:5433/nemr
  said:    error communicating with database: Connection refused (os error 111)

  For a development database, this repository starts one:

      ./scripts/setup_sync_test_db.sh

  For anything else, point DATABASE_URL at a Postgres you run.
  This command does not install or start a database — that is a
  bigger promise than starting a server.
```

The password is redacted in the connection string **and** in the driver's own
error text, so the refusal is safe to paste into a bug thread. Asserted both
ways: the refusal names `127.0.0.1:5999`, and the password never appears.

The error is the real one because the check opens **one connection** rather than
a pool. A pool answers "pool timed out while waiting for an open connection" to
every question, and "connection refused" versus "password authentication
failed" are two entirely different next steps.

## 3. Where the knowledge lives: in the server, not in the client

`nemr-sync --check` is new: the server's own preflight — settings, store,
Postgres — reported as `key=value` lines and then stopped. It migrates nothing
and binds nothing, so it is safe to run against a server that is already up.

```
env_file=/home/nemr/.config/nemr/sync.env
env_file_state=present
env_file_keys=DATABASE_URL,NEMR_SERVER_ADDR,NEMR_BUNDLE_DIR,NEMR_AUTH_PEPPER
settings_state=ok
addr=127.0.0.1:8080
pepper=configured
backend=local:/home/nemr/.local/share/nemr/bundles
backend_writable=yes
backend_state=ok
database=postgres://nemr:***@127.0.0.1:5433/nemr
database_state=ok
```

`nemr server` reads that and speaks it. Two things follow, and both were the
reason for doing it this way:

- **The list of what a server needs cannot drift** between the thing that checks
  and the thing that runs. There is one list, in the binary that needs it.
- **The client does not link the server.** `nemr-cloud` is installed on every
  user's machine; linking `nemr-sync` would put sqlx, the migrations and the
  object-store client in it. The precedent is already in the tree: the client
  drives the open engine as a subprocess rather than linking it.

## 4. `status`: the five questions, answered once

> so when the page says "connection refused" I can ask one question instead of
> five.

```
nemr server
  running:   yes
             pid 216327, started by this command
  address:   127.0.0.1:18099
  /health:   answers
  storage:   local:/tmp/bundles  (answers)
  database:  postgres://nemr:***@127.0.0.1:5433/nemr  (answers)
  pepper:    ephemeral
  settings:  the environment only (NEMR_SYNC_ENV_FILE is empty)
```

It answers for Postgres and the store **whether or not a server is running**, so
"it will not start" and "it is running but broken" are the same one question.

The exit code is the summary: **0 only when it is running and everything it
depends on answered**, so `nemr server status && curl …` means what it looks
like it means. That is a choice, not a requirement of the brief — flagged here
rather than buried.

## 5. First run with nothing configured

It prints the file it wants, with every setting named and explained, and stops.
It writes nothing:

```
nemr server: there is nothing to start from — no settings file, and none in the environment.

The server reads one file — /home/you/.config/nemr/sync.env — mode 0600. …

  # The auth pepper. THERE IS NO DEFAULT AND NOTHING GENERATES ONE FOR YOU:
  # an unset pepper makes /v1/auth/params an account-enumeration oracle that
  # resets on every restart (F-89), so a server without one refuses to bind.
  # Generate it once, into the file, so it never reaches your shell history:
  #
  #   umask 077; mkdir -p "$(dirname ~/.config/nemr/sync.env)"
  #   printf 'NEMR_AUTH_PEPPER=%s\n' "$(head -c 32 /dev/urandom | base64 -w0)" >> ~/.config/nemr/sync.env
  #
  # For a THROWAWAY server only — tests, a demo — the one escape hatch is the
  # literal value below. …
  #
  #   NEMR_AUTH_PEPPER=ephemeral
```

**One reading of the brief you should check.** The trigger is *"the settings
cannot be resolved"*, not *"the file is absent"*. E-19 ruled that a
per-invocation environment beats a persistent file on both halves, and the
script this command was asked to take over — `docs/ui-acceptance.sh` — runs with
`NEMR_SYNC_ENV_FILE=` (read no file at all) and everything in the environment. A
start that refused on file-absence could not be used by the script it replaces.
So: a fully-configured environment starts; nothing configured anywhere gets the
template. Say the word if you want the stricter reading.

## 6. Foreground, and how `stop` can work at all

`start` **execs** the server. The process you started *is* the server: Ctrl-C
reaches it directly, its log is your terminal's, the exit code is its own, and
nothing outlives the window. There is no parent forwarding signals, which is the
part that usually goes wrong.

That leaves `stop` needing an identity. It gets one: a record written before the
exec, 0600, in the client's own state directory, carrying the pid **and the
process's start time from `/proc`**. A reader believes it only when all three
agree — the pid exists, its `comm` is `nemr-sync`, and its start time matches.
Pids are recycled; a stop that trusts the number eventually signals a stranger.

Asserted, with a record naming a live process that is not the server: `stop`
says there is nothing to stop, and the bystander is still alive to say so.
Under the neuter that removes the identity check, the bystander is killed.

`stop` sends SIGTERM and never escalates to SIGKILL. A server that will not stop
is something to look at, not something to shoot.

## 7. Three defects found on the way

**`nemr-sync` had no SIGTERM handler.** It waited on Ctrl-C only, so every
existing caller's SIGTERM — the acceptances', a future systemd unit's, `nemr
server stop`'s — got the default disposition: dead where it stood, mid-request,
with no graceful drain. It now waits on either signal.

**`--check` passed a server that would refuse to bind.** A missing pepper
printed but did not set the exit status, so the preflight said "ready" for the
one condition E-19 ruled must refuse. Fixed, and `--check` now exits non-zero.

**A bundle directory that exists but cannot be written to passed every gate.**
`select_storage` asks only whether it is a directory; the store's probe only
lists, and listing succeeds read-only. The server would start and fail at the
first push. There is now a write probe — local backends only, because the same
probe against an object store costs a class-A operation and leaves an object
behind, which is exactly why E-20 chose a list for reachability.

**A status command started a server.** This one was found by running the
finished command on this machine rather than through the harness, and it is the
worst of the four. `~/.local/bin/nemr-sync` here is yesterday's build, from
before `--check` existed. An older `nemr-sync` does not reject an argument it
does not know — it ignores it and does what it always does: reads the settings
and **starts a server**. So `nemr server status`, which promises to change
nothing, spawned it; that binary opened the developer's real bundle store, bound
a port, and ran for three minutes while `status` sat waiting for a report that
was never coming.

Two things were wrong and both are fixed. The wait is now bounded — twenty
seconds, after which the child is stopped, SIGTERM then SIGKILL — and the report
has to be recognisable: without a `settings_state=` line, whatever ran was not a
preflight, and the refusal says so and names the fix.

```
/home/nemr/.local/bin/nemr-sync did not answer `--check` within 20s, so it was stopped.

  A nemr-sync older than this client does not know that argument and starts a
  SERVER instead, which is the likeliest thing to have just happened. Build or
  install a matching one:

      cargo build --release -p nemr-sync
      ./scripts/install_server.sh

  or point NEMR_SYNC_BIN at the one you mean.
```

Asserted, with a stand-in that has the same shape — a script that ignores its
arguments and does not exit: the refusal arrives, it arrives **on a clock**
(measured at 20s, asserted under 40), it says what is likely wrong, and the
process it started is gone afterwards. A read-only command must not be able to
start a server, and must not be able to wait forever for one.

**And one in a script.** `scripts/sync_acceptance.sh` started the server with no
pepper and without emptying `NEMR_SYNC_ENV_FILE`. Since E-19 a server with no
pepper refuses to bind, so that start worked on this machine only because the
developer's own `~/.config/nemr/sync.env` happens to carry one — it would have
failed on a clean host, which is where that acceptance is supposed to run. It
now starts through `nemr server start` with the ephemeral pepper, like the UI
acceptance.

## 8. And one that was not a defect in this work at all

While running the suites, four of the six lease tests in `crates/nemr-sync`
started failing, each with *"the current holder must be able to write"* — a 409
where a 200 was expected — and a different subset each run.

It is not this change. The control: with `crates/nemr-sync` checked out at the
previous commit, the same tests fail the same way. Serially, they fail too.

**The cause is a clock.** The development Postgres container had drifted **61
seconds ahead of its host**:

| | |
|---|---|
| host | 2026-09-11T01:57:45Z |
| the container | 2026-09-11T01:58:46Z |

The lease is time-based, so a lease taken a moment ago looked long expired to
the database that was asked about it. Restarting the container put the clocks
back within milliseconds of each other, and the six tests passed twice.

Because that cost an hour and read the whole time like a lease bug, `--check`
now measures it — `database_clock_skew_ms`, corrected for half the round trip —
and `status` says so when it exceeds two seconds:

```
  clock:     the database is 61.0s AHEAD of this machine — the lease is
             time-based, so this will look like a lease bug. Restarting the
             database usually fixes it.
```

It is **reported, never enforced**: nothing refuses to start over it. That is an
addition beyond the brief, flagged here rather than buried, and it is asserted
with a stand-in report carrying a minute of skew.

## 9. Both acceptances now start servers the same way

`docs/ui-acceptance.sh` and `scripts/sync_acceptance.sh` both call
`nemr server start`, with `NEMR_SYNC_BIN` pinning the server to the build under
test. The UI acceptance asserts that it went through the command rather than
around it.

## 10. Acceptance

`scripts/server_acceptance.sh` — **43 assertions, all green**, the count itself
asserted. It covers the lifecycle (status, start, status, second start refused,
stop, nothing answering afterwards, record gone), the help's audience sentence,
and every refusal: nothing configured, no pepper, Postgres unreachable, a store
that does not answer, a bundle directory that cannot be written to, and a server
binary too old to understand `--check`. Each
refusal is checked for what it must say **and** for what it must never say.

**The object-store arm did not run here**: `NEMR_S3_BUCKET` is not in this
session's environment, so the run announced the skip and expected five fewer
assertions rather than counting them as passed. The arm is written and needs one
run in your environment — `NEMR_S3_BUCKET` and the rest exported, then
`./scripts/server_acceptance.sh`, which will expect 48. What did run against the
S3 code path is the unreachable-store refusal, with a dead endpoint and dummy
credentials.

### Proved red first

| Neuter | The guard's answer |
|---|---|
| `start` stops printing the template | RED — "the template names every setting" fails |
| `start` stops checking Postgres | RED — "it refuses, naming the connection it tried" fails |
| the server's store probe stops running | RED — "an unreachable store: it refuses, naming provider:bucket" fails |
| `stop` trusts the pid in the record | RED — the innocent bystander is killed |
| `start`'s pepper check **and** its generic backstop are both disabled | RED — "it refuses before starting anything" fails: the run reaches "starting" and takes the address |

The last one is worth the extra line. Disabling only the dedicated pepper check
proved nothing: **three** mechanisms name the pepper — that check, the generic
backstop that prints every error the preflight reported, and E-19's own refusal
inside the server. An assertion that the refusal merely *mentions*
`NEMR_AUTH_PEPPER` therefore cannot fail, and was not measuring what it claimed.
What this command adds is that the refusal arrives **before anything starts**,
and that is what the assertion now says and what the neuter turns red.

## 11. The background mode, as a decision row

`docs/DECISIONS.md` **E-24**, open, with what happens to a server nobody is
watching: where the log goes and who caps it, that a crash is silent until
someone tries to push, that a forgotten server holds the port, and that a
detached server outlives the shell that held its settings. Three options — no
background mode, `--detach`, or a systemd user unit — with a recommendation
(the unit, once there is a deployment ruling; the known blocker is that the
development database has no restart policy, so a unit surviving a reboot would
come up against a Postgres that did not).
