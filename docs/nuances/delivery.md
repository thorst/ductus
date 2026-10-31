# Nuances: messages and delivery

What could surprise you here, why it is so, and what to do. Each entry has a level, 1 (day one) to 5 (expert): the [index](index.md) lists them all by level.

## A message can be delivered twice (level 3)
- **What:** delivery is *at least once*. If the engine crashes (or is killed) after the destination accepted a message but before recording it as sent, the message is sent again on restart. Sent statuses are saved in batches of up to 50, so a crash can re-send up to that many per worker and destination. A clean stop saves them first (see [Stopping the engine](#stopping-the-engine-what-finishes-and-what-waits-level-2) below), so it re-sends nothing.
- **Why:** the alternative (at most once) loses messages. Nothing that was ACKed is ever lost.
- **What to do:** make receivers tolerate repeats (most HL7 receivers dedupe on MSH-10), or use `dedupe` on the receiving interface.

## Your transform can run more than once for the same message (level 3)
- **What:** after a crash, a temporary failure (database down, `fetch()` getting a 503) or an interruption (see [Runaway scripts](runaway.md)), the whole transform runs again from the start. Any `db().execute()` or `fetch()` POST it made the first time happens again.
- **What to do:** make writes idempotent: `INSERT ... ON CONFLICT DO NOTHING`, upserts keyed on the message's control ID, or an idempotency key header on API calls.

## Only the original is stored; outputs are recomputed (level 3)
- **What:** the engine stores each message exactly as received and never changes it. What was sent isn't stored by default; it's reproduced by running the same code on the original. That's exact only if the transform depends on nothing but the message and the code. Lookups (`db()`, `fetch()`), the current time and environment values can give a different result when re-run later.
- **Why:** transforms are fast, and storing every output would cost a database write per message.
- **What to do:** if you must be able to prove exactly what was sent, set `save_output: true` on that destination: what it was sent is kept with its delivery. Keep transforms that don't need lookups free of them. `ductus replay` recomputes a period's outputs on demand and compares them (see Upgrades).

## Stopping the engine: what finishes and what waits (level 2)
- **What:** Ctrl-C, SIGTERM (`systemctl stop`, `docker stop`, Kubernetes) and `ductus drain --stop` all stop cleanly: listeners close, messages being stored get their ACK, each worker finishes the message in hand (a transform or a send under way completes) and saves what it finished, then the engine exits with code 0. A message waiting in a retry back-off or a rate-limit wait isn't sent: it stays first in its lane for the next start. A stop that takes longer than `--stop-timeout` (`DUCTUS_STOP_TIMEOUT`, 30 s) exits anyway, and so does a second Ctrl-C or SIGTERM; then the messages in hand are processed again at the next start, like after a crash. Measured (`scripts/demo-stop.sh`, 6,000 messages over 8 connections, stopped 1 s in; WSL): SIGTERM re-sent nothing in 6 runs and exited with code 0 within 27–42 ms (the stop itself ~15–21 ms); `kill -9` at the same point re-sent 5 messages in one run of 3 (the window is small: workers save after every read). With the receiver down, SIGTERM exited in ~40 ms instead of waiting out the back-off, leaving the 20 messages pending.
- **Why:** an upgrade or a server reboot shouldn't count as a crash.
- **What to do:** prefer `ductus drain --stop` before planned work (it also lets the queues empty first); a plain service stop is just as safe for messages. Keep a transform's `timeout` well under the stop limit, and give the service manager longer than the stop limit (the packages set 45 s; Docker's default is 10 s, so use `docker stop --time 45` or `stop_grace_period`).

## A "done" marker can't prevent duplicates (level 5)
- **What:** recording "this insert happened" in the engine's database doesn't make an insert in *another* database happen exactly once: the two writes can't share a transaction, so a crash between them still repeats the insert on re-run.
- **What to do:** put the guarantee where the data lands: a unique key or upsert in the target table, or an idempotency key the receiving API checks. Use the message's stable key `meta.id` (the second argument of `transform`: `transform(msg, meta)`) for it. It's unique within one engine database; add something per environment if several engines write to the same place.
