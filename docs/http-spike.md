# Spike — a second listener: HTTP on a loopback port beside gRPC on the socket

The Product Owner's question, answered by a spike rather than a design:

> how much of the daemon has to change to serve HTTP on a loopback port
> alongside gRPC on the socket? tonic and axum share hyper, so a second
> listener should be routine — but establish it.

Same discipline as the other spikes: predictions first, then the smallest
change that answers the question, then the measurement of that change. The
code on this branch is the measurement — its diff *is* the answer — and is
not proposed for merging as-is; the HTTP surface's real shape (the handshake,
the routes, where it lives under E-11) comes after.

## Predictions

1. **Change size:** one dependency line (`axum`, already in the workspace via
   `nemr-sync` at 0.8) and under sixty lines in `src/bin/nemrd.rs`; nothing
   in `NemrService`, the proto, or the engine. The HTTP handler reaches the
   engine through the same `Arc<ContainerdClient>` the gRPC service holds.
2. **No version conflicts:** tonic 0.14 and axum 0.8 both sit on hyper 1.x,
   http 1.x, tower 0.5; `cargo tree -d` shows no new duplicate of any of them
   after the change.
3. **Concurrency:** both listeners serve on the daemon's one runtime; a gRPC
   call and an HTTP call in flight at the same time both complete, and the
   HTTP handler's engine call sees the same containerd state the CLI sees.
4. **Shutdown:** the existing signal handling stops both; the socket file is
   removed as today; the port closes.
5. **The seam holds:** `axum` is not a commercial crate; `check_seam.sh` and
   `check_cli_seam.sh` are unchanged and green.
6. **Cost:** binary size grows by a few hundred KB at most; build time by
   seconds.
7. **Port choice** is a policy question, not a mechanism one: the spike binds
   `127.0.0.1:0` and reports the port; a fixed port and the token handshake
   are the next step's design.

## What the spike serves

Two routes, enough to prove the mechanism and nothing that looks like the
product: `GET /health` → `ok`, and `GET /v1/projects` → the same list the
gRPC `List` returns, as JSON, through the engine.

## Measurement (2026-09-05, reference host)

**The change.** `git diff --stat` against main: `Cargo.toml` +3 (axum as a
direct dependency, with a comment), `src/bin/nemrd.rs` +45 / −0, `Cargo.lock`
one line. Nothing in `NemrService`, the proto, the engine, the CLI or the
tests. The HTTP handler reaches the engine through `service.client()`, the
same `Arc<ContainerdClient>` the gRPC service holds, and calls
`project::list` — the function the gRPC `List` calls.

**The graph.** axum 0.8.9 was already in `nemr-engine`'s dependency graph
through tonic; making it direct added no crate. `cargo tree -d` shows no
duplicate of hyper, http, tower or axum before or after: one hyper 1.11.0,
one http 1.5.0, one tower 0.5.3, one axum 0.8.9.

**The run**, a private daemon on its own socket (`NEMR_DAEMON_SOCKET`),
HTTP on an ephemeral loopback port it reported (`127.0.0.1:41333`):

```
/health                        ok
/v1/projects  (HTTP, engine)   7 projects: e2e-smoke-…, htmltest, myproject, netns-acc-a-…, netns-acc-b-…, testing, wphcreate
nemr list --json  (gRPC)       7 projects: the same seven
20 HTTP + 20 gRPC, interleaved in parallel:  HTTP 20 × 200 | gRPC 20 × ok
bound to:                      127.0.0.1:41333 only (ss -ltn)
one SIGTERM:                   port closed (connection refused), socket removed, daemon exited, "[nemrd] shutting down"
```

`check_seam.sh` and `check_cli_seam.sh` both PASS unchanged. Binary size
9,495,688 → 10,413,184 bytes (+918 KB); build time change unmeasurable
against the noise of a warm incremental build.

**Scorecard.**

| # | Prediction | Outcome |
|---|---|---|
| 1 | one dep line, < 60 lines, nothing else touched | Held: 3 + 45 lines; nothing else |
| 2 | no version conflicts, no new duplicates | Held; axum was already in the graph via tonic |
| 3 | concurrent service on one runtime, same state | Held: 20/20 and 20/20 interleaved, identical lists |
| 4 | one signal stops both | Held (the spike aborts the HTTP task after the gRPC server returns; a graceful-shutdown future shared by both is the tidier form for the real surface) |
| 5 | the seam holds | Held, both checks |
| 6 | a few hundred KB, seconds | Half: +918 KB — more than predicted, because axum's own extractors and JSON support are now linked rather than only tonic's use of its router |
| 7 | port policy is design, not mechanism | Unchanged |

One divergence in the *running* of the spike, not in the spike: the load
step's bare `wait` also waited for the daemon started in the same shell, so
the script sat in `do_wait` for five minutes while every request had long
completed. A harness mistake, recorded because it briefly looked like a hang
in the daemon; the daemon answered both listeners throughout.

## What this settles, and what it does not

**Settled:** serving HTTP on a loopback port beside gRPC on the socket is
routine — a second listener on the same runtime, the same engine handle, no
change to the service or the protocol, no dependency conflict, one signal
for both. The daemon-side cost of the HTTP surface is the routes and the
handshake, not the listener.

**Not settled, and not this spike's question:** where the HTTP surface
*lives* under E-11. Everything this spike served is open (the engine's
project list). The account-bearing routes — login, sessions, push, pull —
are commercial, and `nemrd` cannot link them (`check_seam.sh`). So the shape
is still one of: the commercial process serves the whole HTTP surface as a
gRPC client of `nemrd` (the inventory's recommendation), or `nemrd` serves
the open routes and the commercial ones arrive through a seam of the
external-subcommand kind. This spike makes the first cheaper to argue for,
not the second impossible. That, the fixed port, and the token handshake
are the next step's design.
