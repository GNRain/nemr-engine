# Host Prerequisites — Nemr Phase 1

Per Section 3.3 of NEMR-SPEC-001, these components are **provisioned
manually**. The engine does not install its own dependencies in Phase 1.

This document is written to be followed on a clean Ubuntu host by a person
with no prior context on this project. Every command below was executed on the
reference host; the outputs shown are real, not illustrative.

> **Note on Section 3.3 coverage.** Section 3.3 lists containerd, runc, kernel
> support, and rustup. Section 3.7 (PRIV-01) additionally requires containerd
> to run **rootless**, which needs tooling Section 3.3 does not enumerate
> (`uidmap`, `rootlesskit`, `slirp4netns`). Those are included here, since
> Section 3.3 designates this document as the place a clean host is
> provisioned from.

## Do this first

```bash
./scripts/setup_host.sh
```

That script performs every step in this document, in order, idempotently, and
refuses early with a named check, the value it found and the fix when the host
cannot support the stack.

**Read the rest of this file when a preflight check refuses**, or when you want
to know why a step exists. It is the reference; the script is the procedure.
Following it by hand takes about two hours and is how the systemd units below
came to be transcribed by hand into a second host.

The systemd units are **files in the repository**, not text in this document:

| File | Installed to |
|---|---|
| `deploy/systemd/user/containerd-rootless.service` | `~/.config/systemd/user/` |
| `deploy/systemd/user/buildkitd-rootless.service` | `~/.config/systemd/user/` |
| `deploy/systemd/delegate.conf` | `/etc/systemd/system/user@.service.d/` |

## Target host

| Item | Required | Verified on reference host |
|---|---|---|
| OS | Ubuntu 22.04 LTS or newer | Ubuntu 22.04.5 LTS (jammy) |
| Kernel | cgroups v2, overlayfs, unprivileged user namespaces | 6.8.0-136-generic |
| Arch | x86_64 | x86_64 |
| Virtualization | any (reference env is a VirtualBox VM) | VirtualBox 7.2.4 guest |

## Privilege model — read first

Section 3.7 governs. In summary:

- **PRIV-01** — all container operations run against a **rootless** containerd
  owned by your normal user. You are not added to any root-equivalent group,
  and the engine never needs `sudo` to talk to containerd.
- **PRIV-02/03** — volume provisioning (Milestone 3) is the *sole* exception,
  via a narrowly scoped sudoers rule committed at `deploy/sudoers.d/`. That
  rule does not exist yet and is a Milestone 3 deliverable.

`sudo` therefore appears in this document **only** to install packages. Once
setup is complete, no runtime operation in Milestone 1 requires it.

## Step 0 — Verify kernel support (no install required)

```bash
uname -r                                            # expect: 5.8 or newer
stat -fc %T /sys/fs/cgroup                          # expect: cgroup2fs
sysctl kernel.unprivileged_userns_clone             # expect: = 1
sysctl kernel.apparmor_restrict_unprivileged_userns # expect: = 0
unshare --user --map-root-user echo "USERNS OK"     # expect: USERNS OK
grep -H "$(id -un)" /etc/subuid /etc/subgid         # expect: one line each
```

Verified output on the reference host:

```
cgroup2fs
kernel.unprivileged_userns_clone = 1
kernel.apparmor_restrict_unprivileged_userns = 0
USERNS OK
/etc/subuid:nemr:100000:65536
/etc/subgid:nemr:100000:65536
```

Notes on the checks that commonly fail:

- **Kernel must be 5.8 or newer.** The privileged volume helper attaches loop
  devices with the `LOOP_CONFIGURE` ioctl, which landed in Linux 5.8. On an
  older kernel `nemr create` fails with a clear message naming this requirement.
  Ubuntu 22.04 (kernel 5.15) and 24.04 (6.8) both satisfy it; the reference host
  is 6.8. There is deliberately no fallback to the pre-5.8 `LOOP_SET_FD` path —
  it is below the supported floor and untestable here.
- **`apparmor_restrict_unprivileged_userns` must be 0.** Ubuntu 23.10+ ships
  this as `1`, which blocks unprivileged user namespaces and therefore blocks
  PRIV-01 outright. If it reads `1`, that is a genuine blocker to escalate
  under R-06 / E-03, not something to silently override.
- **subuid/subgid ranges must exist** for your user. Ubuntu creates these
  automatically for the first interactive user. If missing:
  `sudo usermod --add-subuids 100000-165535 --add-subgids 100000-165535 $(id -un)`.
- `grep -c overlay /proc/filesystems` reads `0` on a fresh boot. overlayfs is a
  module that autoloads on first use; this is not a missing feature.

## Step 1 — Install packages

Everything is in the Ubuntu archive. No third-party APT repository, and in
particular no Docker repository, is required.

```bash
sudo apt-get update
sudo apt-get install -y \
    containerd runc \
    protobuf-compiler build-essential \
    uidmap rootlesskit slirp4netns fuse-overlayfs
```

| Package | Why |
|---|---|
| `containerd`, `runc` | Section 3.1 runtime stack |
| `protobuf-compiler` | `containerd-client` generates gRPC bindings at build time (see Step 3) |
| `build-essential` | C linker for Rust builds — preinstalled on the reference host |
| `uidmap` | `newuidmap`/`newgidmap`, **setuid** helpers rootlesskit needs to map the subuid range |
| `rootlesskit` | creates the unprivileged user/mount/network namespaces for PRIV-01 |
| `slirp4netns` | rootless networking |
| `fuse-overlayfs` | snapshotter fallback; **not needed** on kernel 6.8, where native overlayfs works in a userns |

Versions installed on the reference host: containerd 2.2.1, runc 1.3.4,
protoc 3.12.4, rootlesskit 0.14.6, slirp4netns 1.0.1, uidmap 4.8.1.

> **Do not install `containerd.io`, `docker.io`, or `docker-ce`.** Those come
> from Docker's repository and pull in Docker Engine, which NFR-01 prohibits at
> any layer. Ubuntu's `containerd` package is Docker-free.

### Disable the system-wide containerd service (required)

Installing the `containerd` package enables a **root-owned** system service.
Phase 1 must not use it — PRIV-01 requires the rootless instance. Disable it:

```bash
sudo systemctl disable --now containerd
```

This is required, not cosmetic. A client that picks up
`/run/containerd/containerd.sock` is talking to the root-owned daemon, and any
acceptance criterion validated against it is void. Disabling the service makes
that mistake impossible rather than merely unlikely — relying on
`CONTAINERD_ADDRESS` being set correctly every time is a weaker guarantee than
the wrong daemon not existing.

Rootless containerd uses the `/usr/bin/containerd` **binary**, not the system
service, so disabling the unit costs nothing.

Verify:

```bash
systemctl is-active containerd            # expect: inactive
systemctl is-enabled containerd           # expect: disabled
ls /run/containerd/containerd.sock        # expect: No such file or directory
pgrep -u root -x containerd               # expect: no output
```

Verified on the reference host:

```
inactive
disabled
ls: cannot access '/run/containerd/containerd.sock': No such file or directory
(no root containerd process)
```

## Step 2 — Set up rootless containerd

### 2a. Delegate cgroup v2 controllers (required to run containers)

By default systemd delegates only `memory` and `pids` to a user session. runc
needs `cpu` (and `cpuset`/`io` for resource bounding) or container start fails:

```
runc create failed: unable to start container process: error during container
init: error setting cgroup config for procHooks process: openat2
/sys/fs/cgroup/.../cpu.weight: no such file or directory
```

Check what is currently delegated:

```bash
cat /sys/fs/cgroup/user.slice/user-1000.slice/user@1000.service/cgroup.controllers
# Insufficient: "memory pids"      Wanted: "cpuset cpu io memory pids"
```

Fix with a root-owned drop-in — this is host provisioning, in the same category
as installing packages, and is **not** the engine escalating its own privileges
(PRIV-01 is unaffected: the daemon and containers still run as your user):

```bash
sudo mkdir -p /etc/systemd/system/user@.service.d
printf '[Service]\nDelegate=cpu cpuset io memory pids\n' \
    | sudo tee /etc/systemd/system/user@.service.d/delegate.conf
sudo systemctl daemon-reload
```

**Then reboot**, or log fully out and back in. The delegation applies when
`user@1000.service` restarts; `daemon-reload` alone does not re-apply it to an
already-running session. Restarting that unit directly would tear down every
user service, including rootless containerd, so a reboot is cleaner.

Verify afterwards:

```bash
cat /sys/fs/cgroup/user.slice/user-1000.slice/user@1000.service/cgroup.controllers
# expect cpu and cpuset to now be present
```

This is required for Milestone 2's AC-2.3 onward, and is load-bearing for the
product's resource-bounded projects generally — not a workaround for one test.

### 2b. Enable lingering

So the service runs without an active login session:

```bash
loginctl enable-linger "$(id -un)"
```

Create `~/.config/systemd/user/containerd-rootless.service`:

```ini
[Unit]
Description=containerd (rootless) — Nemr Phase 1, PRIV-01
Documentation=https://github.com/containerd/nerdctl/blob/main/docs/rootless.md
Documentation=file:///usr/share/doc/containerd/rootless.md

[Service]
Type=simple
Environment=PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
ExecStart=/usr/bin/rootlesskit \
    --state-dir=%t/containerd-rootless \
    --net=slirp4netns \
    --mtu=65520 \
    --slirp4netns-sandbox=auto \
    --slirp4netns-seccomp=auto \
    --disable-host-loopback \
    --port-driver=builtin \
    --copy-up=/etc \
    --copy-up=/run \
    --copy-up=/var/lib \
    --propagation=rslave \
    /bin/sh -c 'rm -f /run/containerd /var/lib/containerd /etc/containerd; \
        mkdir -p %t/containerd /run/containerd && \
        mount --bind %t/containerd /run/containerd && \
        mkdir -p %h/.local/share/containerd /var/lib/containerd && \
        mount --bind %h/.local/share/containerd /var/lib/containerd && \
        mkdir -p %h/.config/containerd /etc/containerd && \
        mount --bind %h/.config/containerd /etc/containerd && \
        exec containerd'

Restart=on-failure
RestartSec=2
Delegate=yes
KillMode=mixed
LimitNOFILE=infinity

[Install]
WantedBy=default.target
```

Then:

```bash
systemctl --user daemon-reload
systemctl --user enable --now containerd-rootless.service
```

### How this works, and why there is no config file

RootlessKit creates unprivileged user, mount, and network namespaces, then runs
containerd inside them. `--copy-up=/etc --copy-up=/run --copy-up=/var/lib`
places a writable tmpfs over each of those directories *inside the namespace*
(the host's copies are untouched), which lets the child bind-mount user-owned
directories over containerd's hardcoded default paths:

| containerd default | bind-mounted from |
|---|---|
| `/run/containerd` | `$XDG_RUNTIME_DIR/containerd` |
| `/var/lib/containerd` | `~/.local/share/containerd` |
| `/etc/containerd` | `~/.config/containerd` |

Because containerd's built-in defaults already point at those three paths, **no
`config.toml` is needed** — the bind mounts redirect them to user-owned storage.

The `%t` and `%h` tokens are systemd specifiers for the runtime directory and
home directory, so the unit is portable across users without editing.

This mirrors the upstream `containerd-rootless.sh` from
[containerd/nerdctl](https://github.com/containerd/nerdctl/blob/v2.3.5/extras/rootless/containerd-rootless.sh),
expressed inline so the whole setup is auditable from this document rather than
depending on an external script. The openSUSE `/etc/ssl` workaround and the CNI
bind-mount from that script are omitted as not applicable here; add
`--copy-up`/bind-mount entries if a later milestone needs them.

### Reaching the socket without nsenter

containerd's packaged `rootless.md` describes entering the daemon's namespaces
with `nsenter` to use a client. **That is not necessary with this setup.**
Because the child bind-mounts `$XDG_RUNTIME_DIR/containerd` onto
`/run/containerd`, the socket containerd creates is the same inode as
`$XDG_RUNTIME_DIR/containerd/containerd.sock`, which is visible in the host
mount namespace. Any client — `ctr`, or the engine — connects to that path
directly.

### When nsenter IS required

The exemption above covers **pure gRPC calls** — version, list, and image pull,
where containerd does all the work server-side. It does **not** cover client
operations that perform a mount themselves.

`ctr container create` is the common example. It mounts the image snapshot
client-side to read the image config, and from the host mount namespace that
fails:

```
$ ctr container create docker.io/library/alpine:latest m1-fixture
ctr: failed to mount ... fstype: overlay ... err: operation not permitted
```

The overlay mount is only permitted inside the user namespace rootlesskit
created. Enter it first:

```bash
CHILD_PID=$(cat "$XDG_RUNTIME_DIR/containerd-rootless/child_pid")
nsenter -U --preserve-credentials -m -n -t "$CHILD_PID" \
    env CONTAINERD_ADDRESS=/run/containerd/containerd.sock \
    ctr container create docker.io/library/alpine:latest m1-fixture
```

The resulting container is then visible from the host namespace over plain
gRPC, because only the *mount* needed the namespace, not the record.

**Rule of thumb:** if the operation only sends a gRPC request, connect directly
to the socket. If the client performs a mount, it must run inside the daemon's
namespaces. This is expected to matter for the engine at Milestones 4–5, where
container creation happens — see the note in README.md.

## Step 3 — Install the Rust stable toolchain via rustup

Do **not** use `apt install rustc cargo` — the archive version lags and
Section 3.1 specifies rustup-managed stable.

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
. "$HOME/.cargo/env"
rustup default stable
```

Reference host: rustc 1.97.1, cargo 1.97.1, rustup 1.29.0.

`containerd-client` 0.9.0 generates its gRPC bindings from `.proto` sources in
a build script, so `protoc` (Step 1) must be present **at compile time**.
Without it the build fails outright:

```
error: failed to run custom build command for `containerd-client v0.9.0`
  Failed to generate GRPC bindings: Could not find `protoc`.
```

## Step 4 — Verification procedure

All of these must succeed, as your normal user, with **no `sudo`**.

```bash
# 1. Binaries
containerd --version && runc --version && protoc --version && cargo --version

# 2. Rootless daemon running
systemctl --user is-active containerd-rootless.service     # expect: active

# 3. Socket exists and is owned by YOU, not root
ls -l "$XDG_RUNTIME_DIR/containerd/containerd.sock"

# 4. API reachable — must print a Server block
export CONTAINERD_ADDRESS="$XDG_RUNTIME_DIR/containerd/containerd.sock"
ctr version
```

Verified output of checks 3 and 4 on the reference host:

```
srw-rw---- 1 nemr nemr 0 /run/user/1000/containerd/containerd.sock

Client:
  Version:  2.2.1
  Go version: go1.24.4

Server:
  Version:  2.2.1
  UUID: b717fc24-7f75-4d81-823e-54fc8668bfe0
```

`srw-rw---- nemr nemr` is the PRIV-01 confirmation: the socket is user-owned,
not `root root`.

> **`ctr version` exits 0 even when the daemon is unreachable**, because the
> Client block still prints. Never use its exit code alone as a reachability
> check — confirm a `Server:` block is present. Against an unreachable daemon
> you get:
> ```
> ctr: rpc error: code = Unavailable desc = connection error:
>   dial unix /run/containerd/containerd.sock: connect: permission denied
> ```

Optionally confirm the snapshotter, which is the main rootless feature risk
(R-06):

```bash
ctr plugins ls | grep snapshotter
```

On the reference host both `overlayfs` and `native` report `ok`, so no
`fuse-overlayfs` fallback is required.

## Explicitly NOT prerequisites

Docker Engine and Docker Desktop are prohibited by NFR-01. If any step here, or
any tool selected later, turns out to require either, that is a release-blocking
defect to be reported under Section 9, not worked around.
