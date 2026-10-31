# The message store and search

Every message the engine accepts is stored in its Postgres database **before** the sender gets
its ACK, as it was received, and kept for the interface's retention. That record is how you
answer "did we get it, when, and what did each receiver get?", and it's where resend, replay and
correction start from.

## What's kept

| | Kept | For |
|---|---|---|
| **The message** | as received, byte for byte, with when it arrived, its ordering key (the MRN) and MSH-10 | the retention window ([retention](retention.md)) |
| **Each destination's delivery** | its status, attempts, last error, when it changed | the same window |
| **What was sent** | only for destinations with [`save_output: true`](../reference/config/interface.md#destinationsave_output); otherwise it's made again by running the code when you look | the same window |
| **What people did** | every resend, replay, edit, dismissal and payload read, with who and why | `audit.keep`, six years by default |

**Only the original is stored by default.** "What did the lab get?" is answered by running the
lab's code on the stored original again, which gives what was sent as long as the code hasn't
changed since. Where you must be able to *prove* what went out (a receiver that disputes it, a
regulator), turn on `save_output` for that destination; it costs about 1.15 GB per million
deliveries ([nuance](../nuances/delivery.md#only-the-original-is-stored-outputs-are-recomputed-level-3)).

An HTTP source in `respond` mode stores nothing: its requests are answered from code, not queued
([nuance](../nuances/ordering.md#respond-mode-http-requests-arent-stored-level-3)).

## Statuses

| Status | A delivery is | A message is |
|---|---|---|
| **pending** | waiting to be sent, or being retried | waiting for at least one destination |
| **sent** | delivered: AA, a 2xx, a complete file, stored in the next interface | finished; at least one destination sent it, none quarantined it |
| **filtered** | not sent on purpose: `null`, `kill()`, dismissed, replaced by a corrected copy, or a destination removed | finished; every destination filtered it |
| **quarantined** | held for a person: refused (AR, a 4xx), the code threw, or `max_attempts` ran out | finished, but needs someone |

A message's status is the worst of its destinations' (quarantined if any is), so "quarantined" on
a message means *some* receiver didn't get it; the message's page says which.

## Finding a message

**In the console**: an interface's **Messages** tab lists its messages newest first, filtered by
status; a message's page shows the payload (with the `payloads` permission), each destination's
outcome and the history. The **Quarantine** page lists every quarantined delivery across
interfaces.

**From the API**: the same list, a page at a time:

```text
$ curl -s -H "Authorization: Bearer $DUCTUS_API_TOKEN" \
    "http://127.0.0.1:7700/api/v1/interfaces/adt-in/messages?status=quarantined&limit=2"
{"messages":[{"id":2,"received_at":"2026-10-31T10:49:28.372890Z","interface":"adt-in","status":"quarantined","ordering_key":"QUARANTINE-ME2","control_id":"QQUARANTINE-ME2","size":117,"last_error":"lab: rejected (AR): rejected by the receiver","updated_at":"2026-10-31T10:49:28.382056Z","origin":null},{"id":1,…}],"next_before":1}
```

`next_before` is the next page's `before`. One message, with its payload and every destination's
outcome, is `GET /api/v1/messages/{id}` ([messages API](../reference/api/messages.md)).

### Searching by content: not yet

**There's no search by MRN, name or field in the console or the API yet.** The message list
filters by status and pages by time; a search across messages by patient and date is planned
(v1.3, with long-term storage). Until then:

- **By the ordering key**: each message keeps its ordering key's value (`ordering_key`, the MRN
  for `ordering.key: PID-3.1`) and MSH-10 (`control_id`) in their own columns. A DBA, or you with
  a read-only login, can find them in the database:

```sql
-- psql, with a read-only login (pg_read_all_data); one interface, one day
SELECT id, received_at, status, control_id
FROM messages
WHERE interface = 'adt-in'
  AND received_at >= '2026-10-30' AND received_at < '2026-10-31'
  AND ordering_key = '123456'
ORDER BY id;
```

  The time range matters: messages are partitioned by time, and with it Postgres reads only that
  day's partitions (checked with `EXPLAIN`); without one it reads every partition. Then open the message in the console by its id (`/messages/<id>`). The tables are
  described in [the database schema](../schema.md).
- **Know what that bypasses**: a payload read through Ductus is recorded in its audit log
  before it's shown; a query in psql isn't. Give such logins only to people your privacy
  officer would let read every message, and prefer selecting ids and keys (as above) over
  `payload`.

Why not yet, and what's planned: [nuance](../nuances/monitoring.md#theres-no-search-by-message-content-yet-level-1).

## Who can see what

- Everyone signed in sees messages' metadata (ids, statuses, errors, keys).
- **Payloads** need the `payloads` permission, and every read is logged before it's shown, once
  per reader per message per 10 minutes ([nuance](../nuances/people.md#payload-reads-are-logged-before-they-are-shown-level-3)).
  The message's page lists **who viewed the payload**.
- **Errors can hold data**: a receiver's AR text or a quarantine reason may quote a field. Write
  quarantine reasons that name the field and the problem, not the patient.
