# The web UI in the Newsreader system — SPEC 1.154

**Date:** 2026-09-11 · **Branch:** `ui-newsreader` (stacked on `arbitrary-volume-size`, PR #95)

The page is restyled to `docs/design/`. The spec (`newsreader-spec/`) is the
system; `nemr.dc.html` is one application of it, and where they differ the spec
won. The prototype's markup and its `<sc-if>`/`<sc-for>` framework were not
adopted: the page is still vanilla HTML and JS in one Rust string literal,
served from the binary.

---

## 1. The fonts, decided

**System stacks. The faces are not embedded.**

The design names Newsreader, IBM Plex Sans and IBM Plex Mono, and the prototype
loads all three from `fonts.googleapis.com`. The page must fetch nothing, so
that was never an option; the question was embed or fall back.

**The files are not here.** They are not in this repository, not in the
`Newsreader design system spec.zip` the design came in (five files: the three
`.dc.html`, `support.js`, a thumbnail), and not installed on the build host —
`fc-list` finds no Newsreader and no Plex. Embedding a font I cannot obtain is
not a decision, it is a wish.

So the stacks name the design's faces first and fall back to what the
prototype's own `font-family` declarations already list:

```css
--serif: "Newsreader", Georgia, "Times New Roman", serif;
--sans:  "IBM Plex Sans", system-ui, -apple-system, "Segoe UI", Roboto, sans-serif;
--mono:  "IBM Plex Mono", ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
```

A machine that has the real faces installed gets them. Everything else gets the
system's, which is what the prototype's fallbacks say to do.

**Embedding stays a contained follow-up**: three woff2 subsets as `data:` URIs
in one `@font-face` block at the top of the stylesheet, and nothing else moves.
The spec sizes them at 48 KB + 42 KB + 38 KB.

### The no-external-resource assertion got real

The old one proved the three assets were **served**:

```sh
for asset in /assets/xterm.js /assets/xterm.css /assets/addon-fit.js; do …200?… done
pass "the page and its terminal are served from the binary (no external resource)"
```

That says nothing about what the HTML *asks for*. A `<link rel="stylesheet"
href="https://fonts.googleapis.com/…">` would have sailed straight past it. So
the claim is now asserted against the served page itself:

```sh
offsite=$(grep -oE '(src|href)="[^"]*"' <<<"$page_html" | grep -vE '"/' || true)
[[ -z "$offsite" ]] || die "the page references something off this origin: $offsite"
grep -qi 'fonts.googleapis.com\|fonts.gstatic.com\|@import' <<<"$page_html" && die …
pass "and the page itself references nothing off this origin (no webfont, no CDN)"
```

Green either way — which is what was asked — but green for the right reason now.

---

## 2. What came from the prototype

**Tokens, verbatim.** The whole `:root` block is the system's `cssTokens`
string: paper, card, rule, ink, coat, eye, ochre, brick, the terminal three.
The names the page already used (`--fg`, `--line`, `--bad`, …) are kept as
aliases pointing at them rather than renamed at every use — a rename is a diff
nobody can read beside a restyle, and they are the same roles.

**Sessions as cards.** One card each: the name in Newsreader 18px, a state
badge beside it, the actions on the right, then the facts —

| | |
|---|---|
| Agent | `claude-code` |
| Disk | `5.8 MiB of 500.0 MiB` |
| Last pushed | `2h ago · studio-mbp` |
| Stored | ▪ ▫ This machine |

— and a 2px quota meter along the bottom. A running session is raised (card
surface, `--rule-strong` edge); one whose bytes are elsewhere is dashed. The
two marks under **Stored** are filled where the bytes actually are; nothing
about where a session lives is signalled by colour, which is the system's own
rule.

**A right-hand column, sticky**, holding exactly one thing: whatever you are
doing, or — when you are doing nothing — what this machine holds. Its three
numbers are counted from the same rows the list is drawn from, so the summary
cannot disagree with the cards above it.

**2px accent focus outlines throughout**, on everything that takes focus, not
only buttons.

**The terminal** on `--term-ground` with the spec's own xterm theme object,
handed over as written.

---

## 3. What stayed ours

The brief's list, each kept verbatim:

- **F-12's two cases.** "remove from this machine" when a bundle exists,
  "remove the only copy" when it does not, with the typed-name confirmation on
  the second. The prototype has the same two panels; ours already had the
  wording and the semantics, so nothing moved.
- **The E-21 no-login banner**, now in the system's ochre.
- **The lease holder.** The prototype has a "Locked" life on the card's badge.
  Ours is a different fact — `held_by` is who holds the lease, and a held
  session can still be running or stopped here — so it keeps its own cell and
  its own ochre, rather than being folded into a badge that would then mean two
  things.
- **The status line**, between two hairlines under the header.

---

## 4. The three things not built

- **The recovery screen.** The prototype says a recovery code "is the only way
  to decrypt sessions you have pushed" — a promise no command in this product
  can perform. Our wording is untouched: *"A forgotten password with no
  recovery code means your data is unrecoverable, permanently: the server
  cannot read it."*
- **"Take lock."** No card gained a take-lock button. The `take_over` checkbox
  on the pull, push and cloud-delete forms is the lease option that was already
  there and is already asserted; D-03's forced takeover still has no UI.
- **The keyboard-shortcut legend.** The prototype's right column ends with
  "Press a session's number to open it… `n` new session, `p` pull, `esc` close
  panel." This page has no shortcuts, so it does not say it has. The markup
  says so where the legend would have gone.

---

## 5. The create panel

#95's slider, not the prototype's four preset buttons, dressed in the system:
an accent track on a `#CFD4D0` rail, the value in mono beside the label, marks
on the track including free disk, a typed field for precision, and the
prototype's note that disk is fixed at creation.

**One bug found by looking at the screenshot:** the free-disk mark and the
`10GB` suggestion landed 10% apart on the log scale and, being absolutely
positioned, rendered as one unreadable label — `10GBe (26.2 GiB)`. A suggestion
that would come within 14% of a mark already placed is now dropped, free disk
first, and the outermost marks align inside the track instead of hanging off
its ends.

**A quota in bytes travels for the first time.** "Used of allocated" needs both
halves and only `used_bytes` was on the wire, so `ProjectStatus` gains
`quota_bytes`, parsed from the container label by the half that owns the size
type. Nothing downstream reimplements a size parser. The page shows
`— of 500.0 MiB` for a volume that is not mounted: the allocation is known even
when the usage is not, and it is the fact the card is about.

---

## 6. Proof the behaviour did not move

`docs/ui-acceptance.sh`, the whole flow through a real browser: **every
existing assertion still green**.

**PASS — 107 assertions, all 107 expected.**

Three changes, all deliberate:

- **One selector.** `tr.srow[data-name=X].children[3]` — a table row's fourth
  cell — becomes `.card[data-name=X] .badge`. Same fact, read from the element
  that now carries it. It is the only DOM hook the restyle moved; every id and
  every `data-*` attribute the acceptance uses is unchanged.
- **One assertion added** (§1): the page references nothing off this origin.
- **The asserted count, 100 → 107.** Only one of those seven is mine. Six are
  SPEC 1.153's — the slider assertions and the picker control it added when the
  quota became a range — which were never counted because that suite could not
  run until a protocol-3 helper existed. This is the first run that could count
  them.

### Two defects the run found, both real

- **The slider's top was a block short of MAX.** `min * exp(ln(max/min))` lands
  a hair under `max` in floating point, and snapping down then loses a whole
  4096-byte block: the end offered 1099511623680 where the helper's maximum is
  1099511627776. The ends are now the bounds, exactly — a bound is not a place
  to be approximately right. The assertion that caught it is one of #95's, on
  its first run.
- **A late list reply could clobber the screen you had moved to.** `showList`
  hides every auth form; a `/sessions` reply that landed after the user logged
  out or opened the register form put the list back or hid the form. The race
  was always there and the restyle widened its window, because the sessions
  reply now also asks the helper for the size bounds. `refresh` carries an
  `authGen` the way panels already carry `panelGen`, and a reply from a screen
  the user has left is dropped.

`docs/ui-screenshots.py` drives the same headless Firefox the acceptance uses
and writes the four screenshots in `docs/design/screenshots/`. It asserts
nothing — it is for looking at before running.

---

## 7. What else ran

| | |
|---|---|
| `cargo fmt --all --check`, `clippy --all-targets` | clean, zero warnings |
| engine unit tests | 165 green |
| `nemr-cloud` unit tests | 28 green |
| `scripts/check_seam.sh` | green |
| `scripts/volume_size_acceptance.sh` (#95) | **32 assertions, green** |

That last one is new: #95 shipped with its acceptance unrun, because the
protocol-3 helper was not installed. It was installed on this host at 17:16
today, so the suite ran for the first time — and found two defects in the
script itself, not the product: `set -e` was killing it at the first
deliberately-failing command substitution, and it reached for a `nemr exec`
that does not exist (the volume is bind-mounted, so `df` on the host mount
point reads the same filesystem the container gets). The asserted count was 24
by my estimate and is 32 by count; the count guard is what caught that.

### One pre-existing failure, not mine

`crates/nemr-cloud/tests/cli.rs::the_server_is_remembered_across_logout_and_a_bare_machine_is_not`
fails. Proven pre-existing by stashing every change in this branch and running
it again at the same commit: it fails there too.
