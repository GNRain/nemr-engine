# Reports

Findings and measurements, one file per report, `YYYY-MM-DD-slug.md`.

They live here rather than in a PR body or a chat message because those arrive
truncated (the Product Owner, 2026-09-10: *"Your last three reports have come to
me truncated mid-sentence. Write findings to a file in the repo and point me at
the path"*). A PR that has a report links to it; it does not reproduce it.

A report is a measurement and its consequences. It is not a decision —
`docs/DECISIONS.md` is where a ruling goes — and it is not a change: a report
that recommends something says so and stops there.

| Report | What it measures |
|---|---|
| [2026-09-11-one-voice-and-one-file.md](2026-09-11-one-voice-and-one-file.md) | sync.env as the only file a server needs, `nemr server configure`, and one voice across both binaries — with every command's before and after |
| [2026-09-11-install-resize.md](2026-09-11-install-resize.md) | Dragging the window stacked the install screen twenty deep: SIGWINCH handled, and the pty harness that can prove it |
| [2026-09-11-server-one-command.md](2026-09-11-server-one-command.md) | One command for the sync server: what it checks, what it refuses, why it will not start Postgres, and the three defects found on the way |
| [2026-09-10-install-icons.md](2026-09-10-install-icons.md) | Colour and one emoji per step in the live region, and the column arithmetic that keeps a two-column glyph from moving the cat |
| [2026-09-10-install-first-run.md](2026-09-10-install-first-run.md) | The first install on a clean distro: an installer that printed nothing at all, a layout that ignored the window, temp files nobody removed, and a log line that had never told the truth |
| [2026-09-10-install-bar-and-pane.md](2026-09-10-install-bar-and-pane.md) | The progress bar that depended on a font, the step-output pane, and what the region costs in rows |
| [2026-09-10-install-default.md](2026-09-10-install-default.md) | The default install screen: one live line and a result rather than a hundred lines of narration — and why the region did not render on WSL2 |
| [2026-09-10-install-screen.md](2026-09-10-install-screen.md) | The install screen as built: 80 columns, live, coloured, and what it leaves on the screen after a failure or Ctrl-C |
| [2026-09-10-install-region-width.md](2026-09-10-install-region-width.md) | How narrow the installer's two-column region could be (80 columns), what that costs, and the first-run tear found while measuring |
| [../gate-audit.md](../gate-audit.md) | What in this repo nothing executes, or whose failure no gate would notice |
