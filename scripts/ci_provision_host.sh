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
sudo install -D -m 0644 deploy/systemd/delegate.conf \
    /etc/systemd/system/user@.service.d/delegate.conf
sudo systemctl daemon-reload

echo "==> Enable lingering so the user manager runs without an interactive login"
sudo loginctl enable-linger "$(id -un)"

# A runner shell may lack these; the user unit needs them.
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
export DBUS_SESSION_BUS_ADDRESS="${DBUS_SESSION_BUS_ADDRESS:-unix:path=${XDG_RUNTIME_DIR}/bus}"
echo "XDG_RUNTIME_DIR=$XDG_RUNTIME_DIR" >> "${GITHUB_ENV:-/dev/null}" || true
echo "DBUS_SESSION_BUS_ADDRESS=$DBUS_SESSION_BUS_ADDRESS" >> "${GITHUB_ENV:-/dev/null}" || true

echo "==> Install the rootless containerd user unit (from deploy/systemd/user/)"
# The unit ships as a file in the repo, not as a heredoc here and not as prose
# in PREREQUISITES.md. It used to be all three, and a human provisioning a host
# hand-transcribed it from the markdown.
mkdir -p ~/.config/systemd/user
install -D -m 0644 deploy/systemd/user/containerd-rootless.service \
    ~/.config/systemd/user/containerd-rootless.service

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

echo "==> Placeholder Claude Code credential (CI only)"
# AUTH-03 makes a missing host credential a hard failure at `create`, by design:
# there is no point provisioning storage for a container that cannot
# authenticate. A CI runner has no real credential, so without this every
# `create` fails and neither the regression suite nor the smoke test can run.
#
# What this placeholder DOES prove: the AUTH-02 mount mechanics — that the file
# is bind-mounted, read-only, at the path Claude Code expects. The smoke test
# asserts exactly that (readable inside the container; `touch` fails with
# "Read-only"), and a placeholder exercises the real code path.
#
# What it does NOT prove: that a real credential authenticates against the API.
# CI runs with NEMR_SKIP_API=1 and the smoke test states which mode it ran in, so
# a CI pass is a visibly weaker claim than a local run. That distinction is
# recorded in docs/CONFORMANCE.md rather than left for a green badge to blur.
#
# Deliberately NOT done: weakening `resolve_credentials` or adding a skip-auth
# flag. That would make CI exercise a different code path than users run, which
# is the failure mode this project keeps hitting.
# Nothing to plant. Until E-21 this wrote a placeholder at
# the host's own Claude directory so the AUTH-02 mount had a source; since E-21 the
# ENGINE writes its own placeholder, at its own path, at create and start
# (F-14 moved that path off ~/.claude, so what this used to write was a file
# the engine no longer reads at all).
echo "    credential: none needed here — the engine writes its own placeholder (E-21, F-24)"

echo "==> Allow unprivileged user namespaces (E-11 offline test)"
# Ubuntu 24.04 ships kernel.apparmor_restrict_unprivileged_userns=1, which blocks
# `unshare -r` for unconfined programs. The E-11 offline test needs a user +
# mount + network namespace to prove that `nemr export` and `nemr import` work
# with no network and no credential, and it REFUSES rather than skips when that
# is unavailable — correctly, since a skipped guarantee is not a verified one.
#
# Scope: this relaxes a hardening knob on a disposable CI runner so a test can
# create its own namespaces. It is NOT a change to how nemr runs anywhere else,
# and it does not touch the privileged helper's scope. A developer host that
# already permits unprivileged userns (the common case) needs nothing.
if [[ -e /proc/sys/kernel/apparmor_restrict_unprivileged_userns ]]; then
    current=$(cat /proc/sys/kernel/apparmor_restrict_unprivileged_userns)
    echo "    kernel.apparmor_restrict_unprivileged_userns = $current"
    if [[ "$current" != "0" ]]; then
        sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0
    fi
fi
if unshare -rmn true 2>/dev/null; then
    echo "    unshare -rmn: available"
else
    echo "    unshare -rmn: UNAVAILABLE — the E-11 offline test will refuse to run" >&2
    unshare -rmn true || true
fi

echo "==> Obtain the base image (pull the published bytes; build only as fallback — F-126/F-127)"
# F-127 (Product Owner ruling): pull first here too, not build. The apt layer is
# unpinned by design, so once the Debian archive moves past a recorded build's
# inputs a fresh build produces a DIFFERENT digest — and building on every CI run
# turned that inevitability into a red F-85 gate on changes that touched nothing
# about the image. That red measured Debian's movement, not our code, and would
# have trained us to ignore the gate. fetch_base_image.sh pulls the recorded
# bytes (the common path now that D-08 publishes them) and only falls back to a
# build when the registry is unreachable — failing closed if that build cannot
# reproduce the recorded digest. build_base_image.sh stays the deliberate publish
# path; drift detection, if wanted, belongs in a scheduled job, not on every PR.
./scripts/fetch_base_image.sh

# A PRIOR published base version, so the F-115 export test has a project whose
# rootfs is provably not this engine's constant (F-116).
#
# Every project the suite creates is built FROM the constant, so without this
# there is no subject that can exhibit the defect and the guard cannot bite —
# which is exactly what was measured before this existed. F-85 keeps every
# published version pullable for ever and forbids reusing a tag, so the ledger
# under image/digests/ is a permanent supply of divergent rootfs.
#
# Pulled here rather than skipped in the test: a test that quietly does not run
# is the failure this project keeps finding.
. "scripts/lib/base_image.sh"
current_version="$(nemr_base_version)"
prior=""
for f in image/digests/*; do
    v="$(basename "$f")"
    [[ "$v" == "README.md" || "$v" == "$current_version" ]] && continue
    prior="$v"
    break
done
if [[ -n "$prior" ]]; then
    echo "==> Pull a prior base version for the F-115 divergent-subject test: $prior"
    ctr -n default images pull --platform linux/amd64 \
        "ghcr.io/gnrain/nemr-base:${prior}" >/dev/null
    ctr -n default images ls | grep -q "nemr-base:${prior}" || {
        echo "pull reported success but ${prior} is not listed" >&2
        exit 1
    }
    echo "    ${prior}: present"
else
    echo "no prior published version in image/digests/ — the F-115 export test" >&2
    echo "cannot build a divergent subject and will fail, by design (F-116)." >&2
    exit 1
fi

echo
echo "Host provisioned. Verify with: ./scripts/verify_wp_a.sh"
