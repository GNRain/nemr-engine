# nemr

Your Claude Code sessions live on one machine. The conversation, the files, the
history — all of it is on the laptop you started on, and moving to another
machine means starting again.

nemr puts each session in its own isolated container with its own disk, and
lets you push that session to object storage you control and pull it down on
another machine with the conversation intact. Sessions are encrypted on your
machine before they are uploaded, so the server stores bytes it cannot read.
There is no Docker at any layer.

> **Private preview.** The source is here for evaluation. It has no
> open-source licence yet, and it is early software — read what works and what
> does not before relying on it.

---

## What works today

- **Sessions.** `nemr create` gives you a container with its own ext4 volume
  and a size limit you choose. Start it, attach to it, stop it, delete it.
  Several run side by side without seeing each other's files.
- **Existing directories.** `nemr add ~/work/thing` copies a directory you
  already have into a session, with its Claude Code history, so it can be moved
  like any other session. The original is copied, never moved.
- **A browser interface.** `nemr ui` opens a page on a loopback port that
  exists only while the command runs. Create sessions, attach to a terminal,
  push, pull, and remove them from the page. The whole flow is exercised
  through the page's own HTTP and WebSocket surface by an acceptance script
  that drives a real Firefox.
- **Cloud sync.** `nemr push` encrypts a session on your machine and uploads
  it; `nemr pull` brings it down on another machine and imports it. The
  encryption key is derived from your password locally and never leaves the
  machine, so a server holds ciphertext it cannot open.
- **One command to install.** `./scripts/install.sh` shows a plan of every file
  it will write and every `sudo` it will run, asks once, then does it.
- **A login per machine.** nemr keeps its own Claude Code credential in its own
  directory. It never copies your `~/.claude`, and a credential never travels
  inside a session.
- **One machine at a time.** A session you push is leased. Another machine that
  tries to push the same session is refused and told which machine holds it,
  and can take over deliberately; after that the first machine's next upload is
  refused by the server, not merely discouraged.

Two things have been measured rather than asserted:

- A session of **107 MiB** exported to a **31 MiB** encrypted bundle.
- On 2026-08-22, a session created on one Ubuntu virtual machine was resumed on
  a second, independently built one: exported, uploaded to object storage,
  downloaded, checksum-verified, imported — and Claude Code then answered a
  question about the order of two earlier instructions, which existed nowhere
  but the transcript. That was one run, by hand, and it has not been repeated.

## What does not work

- **Linux only.** Ubuntu 22.04 or newer, x86_64, kernel 5.8+, cgroup v2,
  systemd, and unprivileged user namespaces. The reference host is a VirtualBox
  guest running Ubuntu 22.04.
- **WSL2 is the answer for Windows, and is not finished.** The engine-level
  blocker is fixed and the installer has been run on a fresh WSL2 distribution,
  but the acceptance that would let it be called supported — a session moved
  between Linux and WSL2 in both directions — has not been run. Treat it as
  unverified.
- **No native Windows build, and none planned. No macOS.** macOS would need a
  virtual machine underneath, which is not in this phase.
- **Claude Code is the agent.** A session can be told to run OpenAI's Codex CLI
  instead, and it launches, but moving a Codex session between machines is
  unverified.
- **One machine at a time, by design.** Two machines cannot work on the same
  session at once. A forced takeover does not yet have a defined answer for
  work the loser had not uploaded.
- **Logging in on a second machine logs the first one out.** Claude Code's
  account has a single refresh token, so using it on another machine spends it.
  nemr warns before you do it; it cannot prevent it.
- **A local `nemr export` / `nemr import` pair has no lease.** Two imported
  copies of one session drift apart silently. The lease exists only when a sync
  server is involved.

## Getting started

nemr does not install these for you:

- **Node.js and Claude Code.** `npm install -g @anthropic-ai/claude-code`, then
  log in once. nemr checks for it and tells you if it is missing.
- **The Rust toolchain.** nemr is built from source:
  `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`.

Then:

```sh
git clone <this repository> && cd nemr-engine
./scripts/install.sh          # shows the whole plan, asks once, then installs
```

It refuses before changing anything if the host cannot run nemr, and names what
is missing. On a fresh machine it installs packages, sets up rootless
containerd, builds the engine and pulls the base image; a cgroup setting needs
one reboot, and it tells you when.

```sh
nemr create myproject         # a session with its own volume
nemr attach myproject         # a shell inside it; run `claude` there
nemr ui                       # or drive it from a browser
```

## The commands

| Command | What it does |
|---|---|
| `nemr create <name>` | Create a session: a size-limited volume and a container ready to start |
| `nemr add <dir>` | Copy an existing directory in as a session, with its Claude Code history |
| `nemr start <name>` | Start the session's container |
| `nemr attach <name>` | Open an interactive shell inside a running session |
| `nemr stop <name>` | Stop the container |
| `nemr list` | Every session, with status and disk used |
| `nemr status <name>` | One session in full: state, volume, base image, credential |
| `nemr delete <name>` | Delete a session and release everything it held |
| `nemr reconcile` | Reclaim mounts, loop devices and snapshots left behind by a crash |
| `nemr export <name>` | Write a stopped session to a portable bundle file |
| `nemr import <bundle>` | Restore a bundle into a session |
| `nemr port add <name> <port>` | Forward a port from the host into a running session |
| `nemr switch-agent <name>` | Change which coding agent the session runs |
| `nemr ui` | Open the browser interface on a loopback port, for as long as it runs |

With a sync server:

| Command | What it does |
|---|---|
| `nemr register` | Create an account, generate the master key, and confirm the recovery code |
| `nemr login` | Log in and store the token and key envelope for this machine |
| `nemr logout` | Revoke the token and remove local state |
| `nemr sessions` | What the server holds, alongside what is local |
| `nemr push <name>` | Encrypt and upload a session; takes the lease |
| `nemr pull <name>` | Download, decrypt and import a session |
| `nemr release <name>` | Give up the lease |

Self-hosting only, and a normal user never runs these:

| Command | What it does |
|---|---|
| `nemr server start` | Check the settings, Postgres and the object store, then run the sync server in the foreground |
| `nemr server stop` | Stop the server this command started |
| `nemr server status` | Whether it is running, on what address, on which storage, and whether Postgres and that storage answer |

## Running your own sync server

`nemr server start` reads one file — `~/.config/nemr/sync.env` — and starts the
server. It does not install or start Postgres: it checks that one answers and
refuses, naming the connection it tried. Run it with no settings and it prints
the file it wants, with every setting explained.

`scripts/install_server.sh` sets up a host from scratch. The details, including
the storage backends and what the server may and may not see, are in
[`docs/DECISIONS.md`](docs/DECISIONS.md) (E-19, E-20) and
[`docs/reports/2026-09-11-server-one-command.md`](docs/reports/2026-09-11-server-one-command.md).

## How it works

- **Rootless containerd and runc.** Sessions are OCI containers run by a
  containerd that belongs to your user, in a user namespace. No Docker, no
  daemon running as root.
- **A loopback ext4 volume per session.** Each session's files live in a file
  on your disk, formatted ext4 and mounted. That is where the size limit comes
  from: it is the filesystem's, not a quota anyone has to enforce.
- **A narrow privileged helper.** Mounting a filesystem needs root. One small
  root-owned program does mount and unmount and nothing else; everything else
  runs as you.
- **Client-side encryption.** A session is compressed and encrypted before it
  leaves your machine, under a key derived from your password. A sync server
  stores objects it cannot read and never sees the key.

## Credits

Nemr is named for a cat who spent his life playing with cables, and he plays
with one on screen while the installer works. That drawing follows an ASCII-art
cat by **Samamine** — same pose, same sparse dotted-outline style. It is redrawn
rather than copied, but the likeness is deliberate, and the credit is theirs.
See `scripts/lib/cat.sh`, or run `./scripts/lib/cat.sh --show`.
