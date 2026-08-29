# Nemr base image — Phase 1, Milestone 2.
#
# Built with a daemonless OCI builder (BuildKit via buildctl, Section 3.1).
# No Docker daemon is involved at any point; see README for the build command.
#
# Base image choice: Debian-slim, via the official node image.
# Section 9's E-01 anticipates escalation *if Alpine exhibits musl libc
# problems* with Node.js or Claude Code — i.e. Alpine is the option carrying
# that risk. Debian-slim (glibc) avoids the failure mode E-01 exists to catch,
# so it is the initial selection and no escalation is required. Alpine remains
# a size optimisation worth revisiting once the engine works end-to-end;
# measured size is recorded in README per AC-2.2.
#
# Node 22 matches the LTS line and the host's own Node (v22.23.2).
#
# Pinned by digest, not by tag (F-74). `node:22-slim` is a moving target: two
# hosts building the same Dockerfile a week apart get different bytes, which is
# how Clawd ended up on sha256:2c4127a5… and Work on sha256:4074f744… from
# identical source. The tag is kept in the reference for readability; the digest
# is what actually selects.
ARG NODE_DIGEST=sha256:d649c27dae7ba0137b3cef5dd75baa422c08dc3d9e3fc0c23dfb172dc3cc6436
FROM node:22-slim@${NODE_DIGEST}

# Runtime dependencies, kept deliberately small:
#   ca-certificates — TLS trust for the Anthropic API; without it Claude Code
#                     cannot authenticate at all.
#   git             — Claude Code's version-control operations in a project.
#   ripgrep         — Claude Code ships a bundled ripgrep, but the bundled
#                     binary is a common source of portability problems.
#                     Installing the distro build and pointing Claude Code at
#                     it avoids depending on the vendored copy.
#   less            — pager used for long output.
#   curl            — a session must be able to diagnose its own network from
#                     inside. Costs ZERO new source packages: libcurl is already
#                     here via git, so this adds only the CLI to a source set the
#                     image already carries.
#   iproute2        — `ip addr` / `ip route`, so "what address am I on" is
#                     answerable in the container whose networking is the newest
#                     and least-exercised part of the system (NET-02..06). NOT
#                     free, and said plainly: it brings iptables, libbpf,
#                     elfutils, libbsd, libtirpc, libmnl and libcap2, widening
#                     this layer's source-package surface by ~42% and therefore
#                     the apt drift recorded below. Judged worth it; if
#                     reproducibility proves fragile because of it, that is a
#                     measurement and it justifies a reversal.
# --no-install-recommends keeps the layer minimal; the apt lists are removed
# in the same layer so they do not persist in the image.
#
# NOT pinned, and this is the honest limit of F-74's fix: `apt-get update` takes
# whatever the Debian mirror currently serves. Pinning it means snapshot.debian.org
# and a snapshot date, which is a further change with its own cost (a frozen
# snapshot stops receiving security updates until someone moves the date). The
# base image and Claude Code were the two inputs that actually drifted here; apt
# is recorded as remaining variance rather than quietly implied to be fixed.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        git \
        iproute2 \
        ripgrep \
        less \
    && rm -rf /var/lib/apt/lists/* \
    # Build residue that embeds the build time in its CONTENTS, which no
    # timestamp rewriting can normalise — measured, not guessed: two cold builds
    # of this layer differed in exactly these files and nothing else. They are
    # logs and caches with no business in a shipped image, so removing them both
    # shrinks it and removes the variance.
    && rm -rf /var/log/apt /var/log/alternatives.log /var/log/dpkg.log \
              /var/cache/ldconfig/aux-cache

# Claude Code itself, pinned (F-74).
#
# It was deliberately unpinned at Phase 1, on the reasoning that the engine is
# what is under test and pinning means rebuilding to pick up updates. That
# reasoning was overtaken by evidence: two hosts built the same Dockerfile and
# got Claude Code 2.1.227 and 2.1.240, which is a different image and therefore
# a base-image drift that M11 correctly refuses to import across. Rebuilding to
# take an update is the cheaper problem.
#
# Overridable, so taking an update is a build-arg away rather than an edit:
#   buildctl build --opt build-arg:CLAUDE_CODE_VERSION=2.1.241 ...
ARG CLAUDE_CODE_VERSION=2.1.240
RUN npm install -g "@anthropic-ai/claude-code@${CLAUDE_CODE_VERSION}" \
    && npm cache clean --force \
    # Same measurement: npm's timestamped debug logs and Node's V8 compile cache
    # were the only files that differed between two cold builds of this layer.
    && rm -rf /root/.npm/_logs /tmp/node-compile-cache

# OpenAI Codex CLI (E-15). A second agent Nemr can run.
#
# One image carries every agent rather than one image per agent: simpler to
# build, publish and reason about, and the incremental cost is small (Codex is
# an npm package on the same Node runtime). Pinned and residue-stripped exactly
# like Claude Code above, so the reproducibility guarantee (F-74) and the
# published-digest check survive the addition — the image is still built from
# fully pinned inputs and is still byte-reproducible.
#
# Overridable the same way:
#   buildctl build --opt build-arg:CODEX_VERSION=0.150.0 ...
ARG CODEX_VERSION=0.149.0
RUN npm install -g "@openai/codex@${CODEX_VERSION}" \
    && npm cache clean --force \
    && rm -rf /root/.npm/_logs /tmp/node-compile-cache

# Use the distro ripgrep rather than the vendored one (see above).
ENV USE_BUILTIN_RIPGREP=0

# The project volume is bind-mounted here by the engine at container creation
# (Milestone 4). Created at build time so the mount target always exists.
WORKDIR /workspace

# Credentials are NOT baked in (AUTH-01). The engine bind-mounts the host's
# credentials directory read-only at container creation (AUTH-02).

CMD ["/bin/bash"]
