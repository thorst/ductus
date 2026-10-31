# Nuances: resending and correcting messages

What could surprise you here, why it is so, and what to do. Each entry has a level, 1 (day one) to 5 (expert): the [index](index.md) lists them all by level.

## Resend sends a message again in place: first in its lane, not last (level 2)
- **What:** resending (`POST /api/v1/messages/{id}/resend`, or many at once with `/messages/resend`) puts a destination's finished delivery back to pending. The message keeps its id, so its worker takes it **next in its key's lane**, ahead of the messages waiting there, but **after** the newer messages for the same key that were already sent. Without destinations named, only the quarantined deliveries are resent. Naming a destination resends to it whatever its delivery ended as, sent or filtered too (a replay: "the lab lost yesterday's results"). Pending deliveries are left alone: they're being sent or retried already. Each resend is recorded with who did it, why, and what the delivery was before (`GET /messages/{id}` shows them).
- **Why:** in place keeps the message's identity and history in one row; nothing is copied.
- **What to do:** for feeds that carry state (ADT updates, results corrections), look before resending an old message: if newer messages for the same patient were sent since, the receiver may apply the old one over newer data. The message's page shows its ordering key; the message browser shows what came after it.

## A replay is the same message; an edited copy is a new one (level 3)
- **What:** a resent message has the same `meta.id` and the same MSH-10, and the transform runs again with **today's** code and lookups (see [Only the original is stored](delivery.md#only-the-original-is-stored-outputs-are-recomputed-level-3)). A receiver that deduplicates on `meta.id` or the control id will drop a replay as a repeat. An edited copy (`POST /messages/{id}/edit`) is a new message: a new id (so a new `meta.id`), its own ordering key and control id read from what you typed, never deduplicated, at the back of its lane. The original stays exactly as received; its quarantined delivery to the copy's destinations becomes filtered ("replaced by edited copy N"), and the copy shows which message it corrects. A routed original's copy keeps its `origin`.
- **What to do:** to replay to a receiver that deduplicates, ask them to accept it (or change MSH-10 in an edited copy). Replaying to a route (`interface: x` destination) queues the message in `x` again, where `x`'s `dedupe` applies.

## A message still pending can't be replaced yet (level 3)
- **What:** an edit is refused for a destination where the original is still pending: its worker may be sending or retrying it at that moment, and both would go. A message that fails with a *temporary* error stays pending and retries forever by default, holding up its key's lane.
- **What to do:** for a destination whose receiver rejects bad data with a temporary error (5xx), set `retry.max_attempts` so such a message ends in quarantine, where it can be corrected. Pausing the destination doesn't make it editable (a send under way finishes, and the message stays pending). A "skip this message" action is planned.
