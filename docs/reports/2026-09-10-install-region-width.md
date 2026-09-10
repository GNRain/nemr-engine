# How narrow the install region could be, and what it costs

**Asked for by the Product Owner, 2026-09-10, on #91:** *"129 columns means most
terminals never see the layout — 80 is the default width. … Measure what the
minimum could be and tell me the trade before changing it — I would rather a
narrower layout most people see than a wide one almost nobody does."*

Nothing here is built. This is the measurement and the trade.

---

## 1. The answer

**The minimum is 80 columns** — exactly the default width — and it needs two
changes, not one. With a one-column gap instead of two it is 79.

| design | left column | gap | cat | **threshold** |
|---|---|---|---|---|
| as shipped (status inline) | 89 *(see §2 — the real figure is 165)* | 3 | 37 | **129** |
| A — status as a right-aligned token | 63 | 2 | 37 | **102** |
| B — A, plus labels trimmed to ≤36 | 48 | 2 | 37 | **87** |
| **C — A, plus labels trimmed to ≤29** | **41** | 2 | 37 | **80** |
| D — C with a one-column gap | 41 | 1 | 37 | **79** |

The cat is fixed at 37 columns (D-14, ruled: the compact variants lose the
likeness), so 37 + a gap is the floor. Everything else has to come out of the
left column, and the left column is `2 (indent) + 1 (glyph) + 1 (space) + label
+ 1 + status`.

**Option A alone does not get there.** Even with the status reduced to a token,
the longest label — `lingering, so the user manager runs without a login`, 51
characters — holds the threshold at 102. The labels are the other half.

---

## 2. What the measurement found first: the shipped threshold is also too NARROW

129 was measured from an idempotent run, where every status is `already done`.
That is the run I had. It is not the run that matters.

| | widest step line |
|---|---|
| a run where everything is already done | **70** |
| **a first run on a clean host** | **145** |

The 145 is the packages step: `✓ packages from the Ubuntu archive — installed:
containerd runc uidmap rootlesskit slirp4netns e2fsprogs build-essential
protobuf-compiler curl`. The status is the package list, and it is 106
characters on its own.

`printf '%-89s'` pads; it does not truncate. So a line longer than the column
pushes the right column out and wraps. Rendered, at 140 columns:

```
  ✓ packages from the Ubuntu archive — installed: containerd runc uidmap rootlesskit slirp4netns e2fsprogs build-essential
  ✓ packages from the Ubuntu archive — installed: containerd runc uidmap rootlesskit slirp4netns e2fsprogs build-essential
  ✓ packages from the Ubuntu archive — installed: containerd runc uidmap rootlesskit slirp4netns e2fsprogs build-essential
 curlhe system-wide root containerd, disabled — disabled                                            \`*-.
  · cgroup v2 controller delegation                                                                     )  _`-.
```

The line wraps, the wrap overwrites the next step's row (`curlhe system-wide…`),
and the cat loses its first row. **The layout tears on a first install — the one
run where a user is watching it.** No assertion caught this because every
acceptance run so far has been an idempotent one.

This is not an argument for a wider column. A column sized to the longest status
string is sized to whatever a step decides to print, which is not a design. It
is an argument for the same change §1 recommends: make the left column's width a
property of the layout, and give the status a fixed, small budget it cannot
exceed.

---

## 3. The trade

### What option C costs

**The labels stop explaining themselves.** Trimmed to ≤29 characters:

| now | at ≤29 |
|---|---|
| `lingering, so the user manager runs without a login` | `lingering` |
| `the privileged volume helper and its sudoers grant` | `helper + sudoers grant` |
| `the engine, built and installed (nemr, nemrd)` | `the engine (nemr, nemrd)` |
| `the rootless containerd and nemrd user units` | `user units` |
| `the system-wide root containerd, disabled` | `system containerd off` |
| `PATH and CONTAINERD_ADDRESS in ~/.bashrc` | `shell environment` |
| `the client CLI (nemr ui, push, pull)` | `client CLI (ui, push, pull)` |
| `the base image ghcr.io/gnrain/nemr-base:0.3.0` | `base image` |

**The status detail leaves the screen.** `installed (7060eab2d8d8)`, `at the
recorded digest 749c092d3a48`, `added (open a new shell, or source ~/.bashrc)`
and the package list all become one of a handful of tokens — `done`, `new`,
`ok`, `pulled`, `failed`.

### What it does not cost

- **The plan already carries all of it.** The block above the question names
  every step in full, every file it writes, every sudo command, the image's
  exact digest and the package list. The step lines are a progress display, not
  the record.
- **The log keeps everything**, as it does now, and is named on failure.
- **The distinction that matters survives.** `done` versus `new` still says
  whether a step did work — which is what "say what it skipped as already done"
  (D-14) asked for.
- **`~/.bashrc` advice.** `added (open a new shell, or source ~/.bashrc)` is the
  one status that is an instruction rather than a fact. It would move to the
  lines printed under the region when it resolves, where the notes already go.

### What it buys

The layout at **80 columns** — the default terminal — instead of 129. And a
left column whose width is fixed by design, so the first-run tear in §2 cannot
happen at any width.

---

## 4. What I would do, and what I need from you

**Recommendation: option C, and treat §2 as the reason rather than a side
effect.** A fixed 7-character status token is what stops a step from setting the
layout's width; trimming the labels is what brings the threshold to 80. Doing
only one of the two leaves it at 102 or 87 — better than 129, still above the
default.

**Option D (79 columns, a one-column gap)** buys a column of margin at the cost
of the two columns reading as one. I would not take it unless you want the
headroom.

**Not recommended: keeping the long labels and widening the column.** It is the
shape that produced §2.

The decision is yours because it is a visible loss: the step names get terser,
and someone reading the block alone learns less than they do today. My weighting
is that the plan above it is where the explaining belongs, and the block is
where the *progress* belongs — but that is a judgement about the product, not a
measurement.

---

## Method

Labels and statuses extracted from `scripts/install.sh` (every `step_def` and
every `tick "$label — …"`), expanded with the values this host produces. Widths
computed as `2 + 1 + 1 + label + 1 + status`. The tear in §2 was reproduced by
publishing a first-run-length step line into a live region at 140 columns and
rendering the transcript through `scripts/lib/render_pty.py`.
