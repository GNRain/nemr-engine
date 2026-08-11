# AI Hub base image — Phase 1, Milestone 2.
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
FROM node:22-slim

# Runtime dependencies, kept deliberately small:
#   ca-certificates — TLS trust for the Anthropic API; without it Claude Code
#                     cannot authenticate at all.
#   git             — Claude Code's version-control operations in a project.
#   ripgrep         — Claude Code ships a bundled ripgrep, but the bundled
#                     binary is a common source of portability problems.
#                     Installing the distro build and pointing Claude Code at
#                     it avoids depending on the vendored copy.
#   less            — pager used for long output.
# --no-install-recommends keeps the layer minimal; the apt lists are removed
# in the same layer so they do not persist in the image.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        git \
        ripgrep \
        less \
    && rm -rf /var/lib/apt/lists/*

# Claude Code itself. Pinned to no specific version at Phase 1: the engine is
# what is under test, and pinning here would mean rebuilding the base image to
# pick up Claude Code updates. Revisit if reproducibility of the image becomes
# a requirement.
RUN npm install -g @anthropic-ai/claude-code \
    && npm cache clean --force

# Use the distro ripgrep rather than the vendored one (see above).
ENV USE_BUILTIN_RIPGREP=0

# The project volume is bind-mounted here by the engine at container creation
# (Milestone 4). Created at build time so the mount target always exists.
WORKDIR /workspace

# Credentials are NOT baked in (AUTH-01). The engine bind-mounts the host's
# credentials directory read-only at container creation (AUTH-02).

CMD ["/bin/bash"]
