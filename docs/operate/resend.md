# Resend, replay and correct

Four things can be done with a message that's already been handled, from the message's page in
the console, the Quarantine page (many at once), or the API. Each is an operator's action,
recorded in the audit log with who, when and the reason. A fifth, `ductus replay`, is different:
it runs a period's messages through code to *compare*, and sends nothing.

| Action | On | Does | Use it when |
|---|---|---|---|
| **Resend** | a quarantined delivery | sends the same message to that destination again | you fixed the cause: the receiver, the code, a code table |
| **Replay** | a sent or filtered delivery | sends the same message to that destination again | a receiver lost what it got, or a destination was filtering wrongly |
| **Edit and resend** | a quarantined message | sends a corrected copy; the original's quarantined deliveries are marked replaced | the message itself is wrong and the sender can't send it again |
| **Dismiss** | a quarantined delivery | takes it out of the quarantine without sending, with a reason | nobody should get it: a test message, a duplicate already handled by hand |

## Resend

After fixing what made a destination quarantine a message, send it again:

```text
$ curl -s -X POST -H "Authorization: Bearer $DUCTUS_API_TOKEN" -H "X-Ductus: 1" \
    -H "content-type: application/json" -d '{"reason":"lab says fixed"}' \
    http://127.0.0.1:7700/api/v1/messages/2/resend
{"id":2,"interface":"adt-in","destinations":[{"destination":"lab","previous_status":"quarantined","previous_attempts":0,"previous_error":"rejected (AR): rejected by the receiver"}]}
```

- **The same message goes again, in place**: first in its lane, not behind everything that came
  since, and with the same id, so a receiver that dedupes still recognizes it
  ([nuance](../nuances/resend.md#resend-sends-a-message-again-in-place-first-in-its-lane-not-last-level-2)).
  It runs the destination's code **as deployed now**: a fixed mapping or code table applies.
- **Without `destinations`** it resends to every destination that quarantined it; the others
  aren't touched.
- **Fix first, then resend.** Resending a message the receiver still refuses quarantines it again
  (the probe above did: the lab still answered AR). The history shows each try.
- **Many at once**: select them on the Quarantine page, or `POST /api/v1/messages/resend` with
  their ids, in one transaction.

## Replay

The same call naming a destination whose delivery was *sent* (or filtered) sends it again:

```text
$ curl -s -X POST -H "Authorization: Bearer $DUCTUS_API_TOKEN" -H "X-Ductus: 1" \
    -H "content-type: application/json" \
    -d '{"destinations":["lab"],"reason":"lab lost it in their upgrade"}' \
    http://127.0.0.1:7700/api/v1/messages/3/resend
{"id":3,"interface":"adt-in","destinations":[{"destination":"lab","previous_status":"sent","previous_attempts":0,"previous_error":null}]}
```

The receiver gets a message it already had: tell them, or be sure they dedupe on MSH-10 (or the
id your code sends). A replay is the same message, not a new one
([nuance](../nuances/resend.md#a-replay-is-the-same-message-an-edited-copy-is-a-new-one-level-3)).

## Edit and resend

When the message is wrong (an MRN mistyped at registration, a segment the receiver can't take)
and the sender can't send a corrected one, correct a copy:

```text
$ curl -s -X POST -H "Authorization: Bearer $DUCTUS_API_TOKEN" -H "X-Ductus: 1" \
    -H "content-type: application/json" \
    -d '{"payload":"MSH|^~\\&|EHR|HOSP|LAB|HOSP|20261031120000||ADT^A08|QFIXED|P|2.5\rPID|1||777001^^^HOSP^MR||SMITH^ANNA\r","reason":"MRN corrected from the EHR"}' \
    http://127.0.0.1:7700/api/v1/messages/2/edit
{"id":3,"original":2,"interface":"adt-in","destinations":["lab"],"replaced":["lab"]}
```

- **The copy is a new message** (id 3 here) going to the destinations that quarantined the
  original (or the ones you name), through their code, as if it had just arrived. The original
  stays exactly as received, and its quarantined deliveries become "replaced" (filtered), each
  pointing at the copy.
- **In the console**, **Edit and resend** opens the payload in an editor, with the destinations
  to send to.
- **A message still pending can't be edited** (it's being retried): wait until it's quarantined,
  or set `retry.max_attempts` on that destination so it gets there
  ([nuance](../nuances/resend.md#a-message-still-pending-cant-be-replaced-yet-level-3)).
- Fix the source too: tell the sending system's team, or the same mistake comes back tomorrow.

## Dismiss

```text
$ curl -s -X POST -H "Authorization: Bearer $DUCTUS_API_TOKEN" -H "X-Ductus: 1" \
    -H "content-type: application/json" -d '{"reason":"test message sent by mistake"}' \
    http://127.0.0.1:7700/api/v1/messages/1/dismiss
{"dismissed":[{"id":1,"interface":"adt-in","destinations":[{"destination":"lab","previous_status":"quarantined","previous_attempts":0,"previous_error":"rejected (AR): rejected by the receiver"}]}],"skipped":[]}
```

A dismissed delivery leaves the quarantine (the destination's error state clears) and becomes
filtered, "dismissed by NAME: reason". **Nothing is deleted**: the message and its history stay
for the retention window ([nuance](../nuances/people.md#dismissing-a-quarantined-message-is-not-deleting-it-level-2)).
A reason is required. Dismiss what nobody should get; don't dismiss what you just haven't had
time for: an error that stays visible is the reminder.

## What each leaves in the audit log

```text
$ ductus audit export --since 2026-10-31 --action message
5 entries
{"id":3,…,"actor":"ops-probe2","via":"token","ip":"127.0.0.1","action":"message.payload.read","subject":null,"interface":"adt-in","message":2,"outcome":"ok","detail":null}
{"id":4,…,"action":"message.release","interface":"adt-in","message":2,"outcome":"ok","detail":{"reason":"lab says fixed","messages":[2],"deliveries":1,"destinations":["adt-in/lab"]}}
{"id":5,…,"action":"message.edit","interface":"adt-in","message":2,"outcome":"ok","detail":{"copy":3,"reason":"MRN corrected from the EHR","messages":[2],"destinations":["adt-in/lab"]}}
{"id":6,…,"action":"message.dismiss","interface":"adt-in","message":1,"outcome":"ok","detail":{"reason":"test message sent by mistake","messages":[1],"deliveries":1,"destinations":["adt-in/lab"]}}
{"id":7,…,"action":"message.replay","interface":"adt-in","message":3,"outcome":"ok","detail":{"reason":"lab lost it in their upgrade","messages":[3],"deliveries":1,"destinations":["adt-in/lab"]}}
```

A resend of quarantined deliveries is `message.release`, a replay of sent ones
`message.replay`. Each message's page shows the same as its **History**.

## Replay a period before a change

`ductus replay` answers a different question: *what would this change do to real traffic?* It
runs a period's stored messages through the code in front of it (a new version, a changed
mapping) and compares what each destination's code decides (sent and what, filtered,
quarantined) with what was recorded, with kept outputs, and with the installed version run beside
it. **It sends nothing and writes nothing**: a read-only login, or a replica, is enough.

```text
$ ductus replay --since 1h
adt-in: 2 message(s) replayed in 0.1 s
ductus replay: version 0.6.0-dev against the record and kept outputs, 2026-10-31 09:48 to 2026-10-31 10:48 UTC, environment dev (nothing was sent)
note: no installed version found (/usr/bin/ductus): outputs compared with kept outputs only; name a binary with --against

adt-in: 2 message(s) in 0.1 s
  ✓ archive: 2 compared, 2 identical
  ✓ lab: 2 compared, 2 identical

2 message(s), 4 comparison(s): 4 identical, 0 different, 0 drift, 0 not comparable, 0 waiting (0.1 s)
```

- **Before an upgrade**, run the new version's binary on the server: `ductus replay --since 24h`
  compares against the installed version and exits 1 on any difference. With `--report
  replay.html` it writes the evidence a change board asks for (`--hide-values` to leave patient
  data out of it) ([nuance](../nuances/upgrades.md#ductus-replay-shows-what-a-change-would-do-to-real-traffic-before-you-make-it-level-1)).
- **Before a code change**, `--baseline <git revision>` compares the project as it is with an
  earlier one: every message whose output the change alters is listed, field by field.
- **It can't compare what code writes** (`db().execute`, an API `POST`): those calls are refused
  in a replay, and the message is marked not comparable
  ([nuance](../nuances/upgrades.md#a-replay-doesnt-write-those-messages-cant-be-compared-level-3)).
- The clock is each message's arrival and `Math.random` repeats, so outputs that use them still
  compare ([nuance](../nuances/upgrades.md#in-a-replay-the-clock-is-the-messages-arrival-and-mathrandom-repeats-level-3)).
- What it costs: [measured](../nuances/upgrades.md#what-a-replay-costs-and-where-level-3) at 100,000
  messages × 3 destinations in 7.5 s.

`ductus replay` re-runs code to compare; resend and replay in the console send. Don't confuse
the two when talking to a change board.
