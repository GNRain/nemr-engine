# Published base image digests (F-85)

One file per published version of `ghcr.io/gnrain/nemr-base`, named for the
version tag, containing the manifest digest that tag was published with.

**These files are immutable.** A version tag names a specific set of bytes and
must never be reused for different ones — so a version's digest file, once
written, is never edited. Publishing a *new* version adds a *new* file; it never
changes an existing one. The build (`build_base_image.sh`) refuses to produce a
digest that disagrees with the recorded one for its version, and the publish
workflow refuses to push different bytes under an existing version.

Why per-file rather than one mapping file: adding a version is an append that
cannot touch another version's entry, and any edit to an existing file — the
exact mistake this guards against — shows as a diff on a file that should never
change, which both the check and code review catch. A single mapping file gets
rewritten on every publish, making an accidental overwrite a one-line diff easy
to miss in review.

| version | contents |
|---|---|
| `0.1.0` | single-agent image (Claude Code only) |
| `0.2.0` | two-agent image (Claude Code + Codex) |
| `0.3.0` | adds `curl` and `iproute2` — a session can inspect its own network |

## A second reason these must not be pruned (2026-08-29)

These files were kept immutable so an old bundle stays importable. They are now
also **the only source of a test fixture**: F-116 needs a project whose rootfs
differs from the engine's current base image, and no project created from the
current constant can be one. `scripts/ci_provision_host.sh` pulls a prior version
from this directory on every host run, and the guard that proves `export` reads
the container rather than the constant goes vacuous without it.

So pruning an old version does not merely make old bundles unimportable — it
silently disarms a test. The provisioning step fails loudly if no prior version
is listed here, rather than skipping.
