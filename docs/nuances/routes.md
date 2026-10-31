# Nuances: routing between interfaces

What could surprise you here, why it is so, and what to do. Each entry has a level, 1 (day one) to 5 (expert): the [index](index.md) lists them all by level.

## A route is exactly once inside the engine, not beyond it (level 3)
- **What:** `destination: { interface: lab }` stores the message in `lab` (which has `source: internal`) in the same database transaction that marks it sent here. A crash before that commit routes it again later; after it, never. Each routed message knows where it came from: `meta.origin` in code, `origin` in the API, `origin_id` in the table. What `lab` then sends to an outside system is at-least-once, as always.
- **Why:** both sides are rows in the same database, so one commit covers both. There's no network hop and no ACK to lose.
- **What to do:** nothing for the hop. Use `meta.id` (or `meta.origin`) as the idempotency key towards outside receivers, as usual.

## The receiving interface's keys, dedupe and destinations apply (level 2)
- **What:** a routed message is a new message of the receiving interface: its `ordering.key`, `dedupe` and destinations are applied as it arrives, read from what the sending function produced. The sending destination's output format must be the receiving interface's `format` (`ductus validate` and startup check).
- **What to do:** if the receiving interface orders by MRN, the routed message must still carry the MRN where its `ordering.key` looks.

## Order holds across a route when the sender keeps it (level 4)
- **What:** messages leave a sending lane in order and are stored in the target in that order. Per-key order holds end to end when the sender orders by the same key, or has no key at all. If several interfaces route to one target, their messages interleave (each sender's own order is kept).
- **What to do:** give the sending interface the same `ordering.key` as the target when order matters.

## A routed message waits up to 20 ms before the next interface sees it (level 4)
- **What:** routes are stored in batches (up to 50 messages, or after 20 ms, or when the worker runs out of work), like sent statuses, so each commit carries many messages.
- **What to do:** nothing, unless a chain of many hops must answer in single milliseconds.

## Routes are fixed in interface.yaml, and loops are refused (level 2)
- **What:** where a destination routes to is configuration, not code. A route that would bring a message back to an interface it passed through (a → b → a) fails `ductus validate` and stops startup. An interface with `source: internal` that nothing routes to only gets a warning.
- **What to do:** to route by content, give the sender a destination per target and `return null` (or `kill()`) in the functions of the ones a message shouldn't go to. Rule-based routing without code comes in v1.1.

## Routes keep moving while draining (level 4)
- **What:** draining stops new messages from outside. Routed messages were already received, so they still flow to their targets, and the drain waits for the targets too.
