# The install acceptance — the arm that needs a clean machine

`scripts/test_install.sh` proves everything about the installers that can be
proved on a host which is already provisioned: every preflight refusal by name,
the plan's completeness, the consent rule in both directions, a second run that
does nothing and says so, and the four rules the animation is held to. It runs
anywhere, takes a few minutes, and asserts its own assertion count.

What it cannot prove is **the first run**. That needs a machine with none of it
present: no containerd, no helper, no sudoers grant, no image, no delegation —
and the reboot that cgroup delegation implies. It needs real `sudo`, a real
GHCR pull, and a real apt transaction. There is no honest way to fake those,
and a faked one would be exactly the "green over nothing" this project keeps
finding, so this arm is a person and a VM.

CI cannot host it either: this repository's Actions quota is exhausted, and
even with it, a hosted runner comes with containerd already installed and
cannot be rebooted mid-job.

**You need:** one Ubuntu 22.04 (or newer) VM and a snapshot you can roll back
to. Every run below starts from the same snapshot.

---

## Snapshot 0 — the machine before nemr

Take the snapshot on a VM that has:

- a normal user with `sudo`, no nemr anything,
- `git` and the Rust toolchain (`rustup`; the installer refuses without it, by
  design — building nemr is not something it installs a compiler for),
- the repository cloned, and nothing else done to it.

Roll back to this snapshot between runs. `nemr` leaves state in
`~/.local/share/nemr`, `~/.local/bin`, `~/.config/systemd/user`,
`/usr/local/libexec` and `/etc/sudoers.d`; a snapshot is the only reliable
undo.

## Run 1 — the first install, on a machine with none of it

```bash
cd nemr-engine
./scripts/install.sh
```

**What to check, in order.**

1. It prints the plan **before** asking anything: the numbered steps, each
   marked `WILL DO`; the files it will write with their modes; every `sudo`
   command; and what it downloads, including the base image's exact digest.
2. It asks once: `Go ahead? [y/N]`. Answer **`n`** the first time. It must say
   `Nothing was changed` and exit 0 — and `~/.local/state/nemr/` must not even
   exist yet. That is the dry run.
3. Run it again and answer `y`. It asks for your sudo password (once, or once
   per privileged step if your sudo timeout is short).
4. It stops with **exit code 3** at the reboot gate, saying cgroup delegation
   is configured but not in effect. This is expected: delegation applies when
   `user@.service` restarts, and restarting it kills the session asking.
   ```bash
   echo $?      # 3
   sudo reboot
   ```
5. After the reboot, run `./scripts/install.sh --yes` again. Everything above
   the gate reports **already done**; everything below it runs: units,
   containerd, the shell block, the build, the helper, the client, the image.
6. It finishes with the smoke test and the two next steps. Total: one command,
   twice, with a reboot in the middle — against a dozen commands across three
   sessions before this.

**Paste back:** the whole output of the run that answered `y`, and the output
after the reboot. The step lines are the evidence; the log path is named in
them only if something failed.

## Run 2 — the second run, proving it is safe to run twice

Immediately after run 1, on the same machine:

```bash
./scripts/install.sh --yes
```

Every step line must read **already done** or **already current**, the smoke
test must pass, and the whole run should take well under a minute — no apt
transaction, no pull, no reinstall. If any line says `installed` or `added`,
that step is not idempotent; that is the finding.

**Paste back:** the `Installing` block.

## Run 3 — a prerequisite missing, proving it refuses and names it

Roll back to snapshot 0, then remove the Rust toolchain from the path the
installer sees:

```bash
env PATH="$(printf '%s' "$PATH" | tr ':' '\n' | grep -v cargo | paste -sd:)" \
    ./scripts/install.sh
```

It must exit **1**, name `the Rust toolchain`, give the `rustup` command that
fixes it, say `nothing has been changed`, and print **no plan at all** — a host
that cannot run this is never half-installed.

Then check that nothing was touched: `ls ~/.local/bin`, `ls /usr/local/libexec`
and `systemctl --user list-unit-files | grep nemr` should all be empty.

**Paste back:** the refusal, and the three `ls` outputs.

## The cat

During the long steps — the packages, the build, the image pull, the smoke test
— a small ASCII cat plays with a cable where a spinner would be. It is drawn by
a separate process, so the install runs at exactly the same speed with or
without it; it disappears when the step finishes, leaving only the tick.

To see the frames without installing anything:

```bash
./scripts/lib/cat.sh --show     # the four frames
./scripts/lib/cat.sh --demo     # animated, then cleared
```

If any of these is true, it draws nothing at all and you get only step lines:
the output is not a terminal, `TERM=dumb`, `NO_COLOR` is set, or `--quiet` was
passed. Ctrl-C at any point must leave the cursor visible and no cat behind.
