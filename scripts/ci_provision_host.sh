#!/usr/bin/env bash
#
# Provision a clean Ubuntu runner to the state PREREQUISITES.md describes:
# rootless containerd (PRIV-01), cgroup v2 delegation, and the base image.
#
# This exists so AC-7.1 — "the smoke test runs on a freshly provisioned Ubuntu
# host with only Section 3.3 prerequisites installed" — is a *tested* claim
# rather than an assertion. It is the same sequence PREREQUISITES.md gives a
# human, scripted; if the two drift, CI is the thing that notices.
#
#   ./scripts/ci_provision_host.sh
#
# Not for developer machines: it assumes a disposable runner and takes
# liberties (systemd --user via a lingering session) that a real workstation
# would already have configured.

set -euo pipefail

echo "==> Packages (Ubuntu archive only — no Docker repository, NFR-01)"
sudo apt-get update
sudo apt-get install -y \
    containerd runc \
    protobuf-compiler build-essential \
    uidmap rootlesskit slirp4netns

echo "==> Disable the system-wide root containerd (PRIV-01: we use the rootless one)"
# Left running it would still answer on /run/containerd/containerd.sock and a
# mistake in socket resolution could silently connect to the root daemon —
# exactly the class of "green but wrong" this project keeps hitting.
sudo systemctl disable --now containerd.service 2>/dev/null || true

echo "==> subuid/subgid ranges for $(id -un)"
grep -q "^$(id -un):" /etc/subuid || sudo usermod --add-subuids 100000-165535 "$(id -un)"
grep -q "^$(id -un):" /etc/subgid || sudo usermod --add-subgids 100000-165535 "$(id -un)"

echo "==> cgroup v2 controller delegation (memory+pids are default; cpu/cpuset/io are not)"
# Without cpu delegated, rootless task start fails on `cpu.weight: no such file
# or directory` (SPEC §3.3, PREREQUISITES Step 2a).
sudo mkdir -p /etc/systemd/system/user@.service.d
sudo tee /etc/systemd/system/user@.service.d/delegate.conf >/dev/null <<'CONF'
[Service]
Delegate=cpu cpuset io memory pids
CONF
sudo systemctl daemon-reload

echo "==> Enable lingering so the user manager runs without an interactive login"
sudo loginctl enable-linger "$(id -un)"

# A runner shell may lack these; the user unit needs them.
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
export DBUS_SESSION_BUS_ADDRESS="${DBUS_SESSION_BUS_ADDRESS:-unix:path=${XDG_RUNTIME_DIR}/bus}"
echo "XDG_RUNTIME_DIR=$XDG_RUNTIME_DIR" >> "${GITHUB_ENV:-/dev/null}" || true
echo "DBUS_SESSION_BUS_ADDRESS=$DBUS_SESSION_BUS_ADDRESS" >> "${GITHUB_ENV:-/dev/null}" || true

echo "==> Install the rootless containerd user unit (verbatim from PREREQUISITES.md)"
mkdir -p ~/.config/systemd/user
cat > ~/.config/systemd/user/containerd-rootless.service <<'UNIT'
[Unit]
Description=containerd (rootless) — Nemr Phase 1, PRIV-01
Documentation=https://github.com/containerd/nerdctl/blob/main/docs/rootless.md

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
UNIT

systemctl --user daemon-reload
systemctl --user enable --now containerd-rootless.service

echo "==> Wait for the rootless socket"
sock="${XDG_RUNTIME_DIR}/containerd/containerd.sock"
for _ in $(seq 1 30); do
    [[ -S "$sock" ]] && break
    sleep 1
done
if [[ ! -S "$sock" ]]; then
    echo "rootless containerd did not come up; unit status:" >&2
    systemctl --user status containerd-rootless.service --no-pager || true
    journalctl --user -u containerd-rootless.service -n 50 --no-pager || true
    exit 1
fi
export CONTAINERD_ADDRESS="$sock"
echo "CONTAINERD_ADDRESS=$sock" >> "${GITHUB_ENV:-/dev/null}" || true
ctr version

echo "==> Install BuildKit (daemonless OCI builder, SPEC §3.1)"
# Not in the Ubuntu archive, so it comes from the upstream release tarball —
# pinned to the version the reference host uses, so CI and a developer build the
# base image with the same builder. The checksum is verified: this is the one
# binary the provisioning path fetches from outside the distribution, and an
# unverified download would be the weakest link in an otherwise Docker-free,
# archive-only supply chain.
BUILDKIT_VERSION="${BUILDKIT_VERSION:-0.32.2}"
BUILDKIT_SHA256="${BUILDKIT_SHA256:-2975d0f651ad96ba8b80b9992ae1f9a964f4408569af5b6dc36544165c3926af}"
mkdir -p "$HOME/.local/bin"
if ! command -v buildctl >/dev/null 2>&1; then
    tarball="buildkit-v${BUILDKIT_VERSION}.linux-amd64.tar.gz"
    url="https://github.com/moby/buildkit/releases/download/v${BUILDKIT_VERSION}/${tarball}"
    curl -fsSL --proto '=https' --tlsv1.2 -o "/tmp/${tarball}" "$url"
    if [[ -n "$BUILDKIT_SHA256" ]]; then
        echo "${BUILDKIT_SHA256}  /tmp/${tarball}" | sha256sum -c -
    else
        # No pinned digest supplied: record what was fetched so a change in the
        # upstream artifact is at least visible in the log rather than silent.
        echo "    WARNING: BUILDKIT_SHA256 not set; fetched digest is $(sha256sum "/tmp/${tarball}" | cut -d' ' -f1)"
    fi
    tar -xzf "/tmp/${tarball}" -C /tmp
    install -m 0755 /tmp/bin/buildctl /tmp/bin/buildkitd "$HOME/.local/bin/"
    rm -rf "/tmp/${tarball}" /tmp/bin
fi
export PATH="$HOME/.local/bin:$PATH"
echo "PATH=$HOME/.local/bin:$PATH" >> "${GITHUB_ENV:-/dev/null}" || true
buildctl --version

echo "==> Build and import the base image (BuildKit, daemonless — no Docker)"
./scripts/build_base_image.sh

echo
echo "Host provisioned. Verify with: ./scripts/verify_wp_a.sh"
