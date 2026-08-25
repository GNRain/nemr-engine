# Nemr

**Nemr gives each of your projects its own isolated Claude Code workspace on
one machine — and lets you pack a project into a single file and move it to
another machine, with the conversation intact.**

You keep several projects running side by side, each with its own files and its
own private disk. When you switch computers, you export a project to a file,
carry it across, and import it. Claude picks the conversation back up.

> **Status: private preview.** Nemr is not publicly released and has no open-source
> license yet — the source is shared for evaluation only. It runs on Linux, it
> has no graphical interface, and it is driven entirely from the terminal. The
> engine does its whole job end to end, but it is early software: see
> [Known limitations](#known-limitations) before relying on it.

---

## Contents

- [What it does](#what-it-does)
- [Getting started](#getting-started)
- [The commands](#the-commands)
- [Moving a project to another machine](#moving-a-project-to-another-machine)
- [What you need](#what-you-need)
- [How it works](#how-it-works)
- [Where the project is today](#where-the-project-is-today)
- [Known limitations](#known-limitations)
- [Uninstalling](#uninstalling)
- [Getting help](#getting-help)
- [For engineers](#for-engineers)

---

## What it does

**Keeps projects in separate boxes.** Each project gets its own private disk —
one file on your computer that the project sees as a separate drive, with a size
limit you choose. A project only ever sees its own disk, so your projects don't
get tangled together.

**Remembers the conversation.** Claude Code's history lives inside the project.
Stop a project today, start it next week, and the conversation is still there.

**Travels.** A project can be packed into one file and unpacked on a different
computer — as long as that computer has Nemr set up too. This has been done end
to end once, by hand, between two separately built machines: the conversation
continued on the far side, in order, with nothing re-typed.

**Runs as you.** Nemr uses the same permissions as any app you run. It needs
your password once during setup, and briefly each time it sets up a disk — and
it prints a line telling you whenever that happens. It does not need Docker.

---

## Getting started

Setup is a one-time thing and takes roughly an hour, mostly unattended — it
installs some system pieces, builds the starter image every project is based on,
and **reboots your machine once** in the middle. After that, working with
projects takes seconds.

Nemr is private preview, so you first need access to the source. If you have it:

**1. Set up your machine** — one command, safe to run again:

```bash
./scripts/setup_host.sh
```

It first checks your machine can run Nemr and **stops without changing anything**
if it can't, telling you exactly what's wrong and how to fix it. Part-way through
it asks you to reboot; run it again afterwards and it continues where it left
off. When it finishes, the `nemr` command is installed — check with `nemr --help`.

**2. Log in to Claude Code** (once per machine — install Claude Code first if you
haven't):

```bash
claude
```

**3. Make a project and open it.** Run `nemr create` on its own to be walked
through the name, size and agent (Claude Code or Codex), or give them directly:

```bash
nemr create myproject --size 2GB --agent claude-code
nemr start myproject
nemr attach myproject
```

You're now inside your project — an ordinary terminal prompt. Run `claude` and
work normally. Press `Ctrl-D` to leave; the project keeps running until you
`nemr stop` it.

---

## The commands

| Command | What it does |
|---|---|
| `nemr create <name> --size 2GB` | Make a new project. Pick one of three sizes: `500MB`, `2GB`, `10GB` (permanent — see [limitations](#known-limitations)), and which coding agent it runs. |
| `nemr switch-agent <name> <agent>` | Change which agent a stopped project runs. |
| `nemr start <name>` | Start it up. |
| `nemr attach <name>` | Open a terminal prompt inside it. |
| `nemr stop <name>` | Shut it down. Your files stay. |
| `nemr list` | Show every project, whether it's running, and how much space it uses. |
| `nemr status <name>` | Everything about one project in one place. |
| `nemr export <name>` | Pack a project into a single file (`<name>.nemr` in the current folder) to carry elsewhere. |
| `nemr import <file>` | Unpack one, creating the project automatically. |
| `nemr delete <name>` | Remove a project and everything in it. |
| `nemr reconcile` | Clean up leftovers after a crash or power cut. `nemr list` warns you when it's needed. |

Add `--verbose` to any command to see exactly what it's doing under the hood.

### What `status` tells you

```
myproject
  state:        running          (running, or stopped)
  usage:        76.0KiB of 500MB (0%)
  loop device:  /dev/loop19      (the technical handle for this project's disk)
  mount check:  ok               (confirms the right disk is attached, not a stray one)
  credential:   present at ~/.claude/.credentials.json, last written 0 days ago
```

The last two lines exist because "is the right disk attached?" and "do I have a
valid login?" were the two questions that used to take several commands and a
lot of guessing to answer.

---

## Moving a project to another machine

On the machine you're leaving:

```bash
nemr stop myproject
nemr export myproject          # writes myproject.nemr in the current folder
```

Copy that one file across however you like — a USB stick, `scp`, cloud storage.
**Nemr doesn't do the copying for you**, and the file is not encrypted, so treat
it like any other file that holds your work.

On the machine you're arriving at (it must have Nemr set up, and the same starter
image — import will tell you clearly if it doesn't):

```bash
nemr import ~/Downloads/myproject.nemr
claude                          # log in first — the file never carries your login
nemr start myproject
nemr attach myproject
```

The file remembers the project's name and size, so you don't have to.

**Two things worth knowing before you rely on this:**

- **Your Claude login never travels with the file** — by design, so a file you
  might email yourself can't leak your credentials. Log in on the new machine.
- **Import makes an independent copy.** If you keep working on *both* machines,
  the two copies drift apart and nothing merges or warns you. Treat a project as
  living on one machine at a time.

---

## What you need

- **Recent Ubuntu Linux** — 22.04 or newer. Not sure? The setup script checks
  for you and explains if anything's missing. Nemr does **not** run on macOS or
  Windows.
- **Your password (sudo) for setup only** — to install system pieces and one
  small helper. Day-to-day use doesn't need it.
- **Claude Code installed, and a subscription** — you log in on each machine you
  use ([Claude Code install guide](https://docs.anthropic.com/en/docs/claude-code)).
- **A few hundred MB of free disk for setup**, plus whatever you give your
  projects. Note that project disks are thin — a `2GB` project doesn't take 2 GB
  until you fill it (see [limitations](#known-limitations)).

Nemr costs nothing to use; you only need your existing Claude Code subscription.

---

## How it works

Each project is three things:

1. **A private disk** — think of it as a virtual USB stick: one ordinary file on
   your computer that the project sees as a separate drive with a fixed size
   limit. This is what keeps projects apart.
2. **A container** — a lightweight sandbox holding Claude Code and its tools,
   with the project's disk attached, and your Claude login attached read-only.
3. **A record** — Nemr keeps no list of its own. It relies on the underlying
   Linux plumbing (a standard component called `containerd`) and the machine
   itself as the only sources of truth, so there's no private database to drift
   out of sync with reality.

**A background service does the work.** `nemr` commands talk to a small
background service (`nemrd`) that manages your projects; it starts automatically
the first time you run a command. Keeping one service in charge means two
commands can never trip over each other. If it isn't running, the next command
starts it — you never have to.

**On the isolation:** a project can only see its own disk — no other project's
files are attached to it. This is ordinary container separation, good for
keeping work tidy and independent. It has **not** been hardened or reviewed as a
security boundary, so don't rely on it to contain untrusted code (see
[limitations](#known-limitations)).

---

## Where the project is today

**The engine does the whole job, end to end.** Every command in the table above
works and is covered by 152 automated tests that run on every pull request and
every merge, on a clean machine built from scratch each time.

It is **early software, not finished** — the [conformance
ledger](docs/CONFORMANCE.md) lists what's still open, and the
[limitations](#known-limitations) below cover what you'd actually run into.

### What's coming next

- **A background service** — so a session stays recoverable while nothing is
  attached, and as the foundation for anything with a window and buttons.
- **A security review, then open source** — the core is intended to become open
  source, after a proper review rather than before.
- **A website for your projects** — log in, see your projects across your
  machines, pull one down and open it. This is the part that turns the tool into
  a product.

---

## Known limitations

Written plainly, because finding these out by surprise is worse than reading
them here.

- **Codex support is unverified.** You can create a Codex project and Codex
  runs, but whether a Codex conversation survives a stop/restart or travels in
  an export has **not been tested** — only Claude Code has. Nemr warns you when
  you pick Codex. Treat a Codex project's history as not yet safe to rely on.
- **No external security review yet.** Setup installs a passwordless `sudo` rule
  for one small root-owned helper. The project separation above is not a security
  boundary — don't run code you don't trust inside a project.
- **Linux only.** No macOS, no Windows.
- **One machine at a time.** In the one case observed, logging in to Claude Code
  on a second machine revoked the login on the first. We don't yet know how
  general that is; treat a project as living on one machine at a time.
- **A login that expires while a project is running** isn't picked up inside the
  running project. Today the only fix is to delete and recreate the project —
  which loses the container (your files in it survive; export first if unsure).
  A better fix is planned.
- **Project size is permanent.** You pick `500MB`, `2GB` or `10GB` at creation
  and can't change it later.
- **Project disks are thin-provisioned, and Nemr won't stop you overcommitting.**
  A `2GB` project only takes space as you use it, but nothing prevents you
  creating more project space than your disk holds. If your disk fills, projects
  can fail to write.
- **Only your project files and conversation travel.** Anything you install
  *inside* a project (system packages, global tools) does not go in the export —
  only the project's own disk does. And a git repository is exported without its
  full history (its object store is left out to keep the file small).
- **`nemr stop` occasionally fails on a busy machine** (seen once in twenty
  runs), leaving a project that reports an error instead of stopping. Undiagnosed.
- **A bundle doesn't carry the starter image.** The machine you import onto must
  already have it; Nemr won't download it for you (import says so plainly if it's
  missing).
- **Nemr never copies files between machines for you.** Export gives you a file;
  moving it is up to you.

---

## Uninstalling

There's no uninstall command yet. To remove Nemr by hand:

```bash
nemr list                            # note your projects
nemr delete <name>                   # for each, to release its disk (export first to keep anything)
rm ~/.local/bin/nemr                 # the command
sudo rm /usr/local/libexec/nemr-volume /etc/sudoers.d/nemr-volume   # the helper and its grant
```

Setup also installed system packages, two background services and a reboot-time
setting; those are standard components and are left in place. `PREREQUISITES.md`
lists exactly what was changed.

---

## Getting help

Something wrong? `nemr status <name>` and `nemr --verbose <command>` show what's
happening, and `nemr reconcile` clears leftovers after a crash. Beyond that,
this is private-preview software — report issues to the person who gave you
access.

---

## For engineers

| Document | What's in it |
|---|---|
| [`docs/ENGINEERING.md`](docs/ENGINEERING.md) | How every part was built, measured and verified. |
| [`PREREQUISITES.md`](PREREQUISITES.md) | What each setup step does and why. Read when a check refuses. |
| [`SPEC.md`](SPEC.md) | The specification and its full revision history. |
| [`docs/DECISIONS.md`](docs/DECISIONS.md) | Every significant decision, its reasoning, and what it cost. |
| [`docs/CONFORMANCE.md`](docs/CONFORMANCE.md) | Every defect found, how it was proven, and its disposition — fixed, open, or refuted. |
| [`docs/bundle-format.md`](docs/bundle-format.md) | The file format used to move a project. |

```bash
cargo test --workspace --lib                        # host-free unit tests
cargo test --test regression -- --test-threads=1    # full suite (needs a set-up host)
./scripts/e2e_smoke_test.sh                         # end-to-end
./scripts/verify_wp_a.sh                            # acceptance
```

[![publish base image](https://github.com/GNRain/nemr-engine/actions/workflows/publish-base-image.yml/badge.svg)](https://github.com/GNRain/nemr-engine/actions/workflows/publish-base-image.yml)

The base image is pushed to `ghcr.io/gnrain/nemr-base:0.2.0`. It builds
reproducibly — the same source produces the same image, byte for byte, given the
same package snapshots. It is **not yet anonymously pullable** (the GHCR package
is private pending a one-time visibility change); `scripts/check_base_image_published.sh`
checks and explains.
