# Nuances: operations: drain mode and the REST API

What could surprise you here, why it is so, and what to do. Each entry has a level, 1 (day one) to 5 (expert): the [index](index.md) lists them all by level.

## A start logs one line, not one per listener (level 2)
- **What:** an engine's start ends with one line: `started interfaces=1001 mllp=1001 http=0 files=0 schedules=0 internal=0 shared=1001 pool_threads=8 dedicated_threads=0 took=712ms`. Since v0.6 the per-interface lines ("listening (MLLP)", "watching for files", "schedule started", and "MLLP listener closed" at a stop or drain) are debug lines: a thousand-interface engine wrote a thousand of each at every start, stop and drain. Measured with 1,001 interfaces (WSL): ready in 0.69–0.83 s warm (1.3 s with a cold file cache), down from ~5 s in v0.5.
- **What to do:** `ductus ports` lists what a project listens on. To see each listener in the log, `RUST_LOG=info,ductus_runtime=debug`; that also logs how long each step of the start took ("start: ...").

## Draining closes MLLP listeners; it doesn't answer AE (level 4)
- **What:** `ductus drain` (or `POST /api/v1/drain`) stops taking new messages. MLLP listeners close: open connections are closed after the message being handled is ACKed, new ones are refused. A message that was half received, or that arrived just as the drain started, is dropped unanswered, never ACKed, so the sender sends it again later. Queued HTTP sources answer 503 with `Retry-After: 30`. File pickup pauses (a file being read stops between batches and resumes at its saved offset after `ductus resume` or a restart). A schedule that comes due while draining runs once on resume, however many times it came due.
- **Why:** to a sender, a closed port means "receiver down: keep the message and retry", which every sender handles. An AE can be configured to mean "skip it after N tries" on some engines, which would lose messages.
- **What to do:** expect senders to show the connection as down during a drain. Use it for upgrades and server moves: `ductus drain --wait --stop`, upgrade, start.

## Respond-mode HTTP keeps answering while draining (level 4)
- **What:** respond-mode interfaces (APIs that answer from a transform) keep answering until the engine stops. They store nothing, so there's nothing to drain.
- **What to do:** take the server out of your load balancer when you drain it. `GET /api/v1/health` reports `drain: draining`, which a load balancer can check.

## A drain finishes only when every destination has caught up (level 4)
- **What:** "drained" means nothing is pending anywhere. A destination that's down, or an interface stopped because its code ran away, keeps the drain waiting (`ductus drain --wait` and `GET /api/v1/drain` show which interfaces). Quarantined messages don't count: they wait for a person anyway.
- **What to do:** fix the destination, or resume and plan the maintenance for later. Stopping anyway is safe: what's pending stays stored and goes out after the restart.

## Drain mode isn't remembered across a restart (level 4)
- **What:** an engine that restarts takes messages again, even if it was draining.
- **Why:** the usual sequence is drain, stop, upgrade, start, and the start should take messages.
- **What to do:** to keep senders out across a restart, stop the senders, or drain again right after starting.

## The API listens on this machine only, until you open it (level 4)
- **What:** the REST API listens on `127.0.0.1:7700` by default (`api.listen` in ductus.yaml, `--api-listen`, or `off`). Every call except `/health` and `/openapi.json` needs a token. Tokens are made on the server with `ductus token create` and are stored only as hashes: a lost token can't be shown again; revoke it and make a new one.
- **What to do:** before opening it to the network, give it `tls:` (see [The API on the network needs `tls`](tls-listening.md#the-api-on-the-network-needs-tls-level-1)), or put a TLS reverse proxy in front and keep it on `127.0.0.1`.
