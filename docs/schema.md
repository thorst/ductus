# Database schema

What Ductus keeps in PostgreSQL, for transparency (and for DBAs tuning the server). **This is not an API.** Tables and columns can change in any release; read and change data through the REST API (`/api/v1`) and the `ductus` commands, which keep working across versions. Queries written against these tables may break on upgrade.

Changes only ever add (migrations in `crates/ductus-store/migrations`, see [architecture.md](architecture.md) invariant 4), so an older engine still runs on a newer schema: that's what makes rollback safe. A test checks that every table and column is described here.

Statuses (`status` columns): `0` pending, `1` sent, `2` filtered, `3` quarantined, `4` duplicate (messages only).

## `messages`

Every message received, as received (the original is never changed). **Partitioned by retention tier, then by time** (migration 0010): `messages_t30` holds the 30-day tier, and its partitions cover a period each, named by their first day (`messages_t30_20261008`). Periods are sized to the tier: a day for 7d and 30d, a week (from Monday) for 90d and 180d, a month for 365d, so there are about 100 partitions in all. They're created 3 days ahead; a partition is dropped once its whole period is older than its tier's days (see `retention`).

| column | type | |
|---|---|---|
| `id` | bigint | From a sequence: arrival order within an interface. With `received_at`, the primary key (a partitioned table's key must include the partition column). |
| `received_at` | timestamptz | When it was stored. |
| `interface` | text | The interface's name. |
| `status` | smallint | Overall status: pending until every destination is done; then quarantined if any destination quarantined it, else sent if any sent it, else filtered. |
| `partition_key` | integer | The lane, from the ordering key (`ductus_core::partition_of`). |
| `ordering_key` | text | The ordering key's value (e.g. the MRN), if the interface has one. |
| `control_id` | text | MSH-10 for HL7, when present. |
| `payload` | bytea | The message as received. |
| `attempts` | integer | Used before status per destination (v0.5); attempts are now counted per delivery. Kept for older engines; removed when the migrations are squashed before release. |
| `last_error` | text | Why it was quarantined or filtered (`destination: reason; ...` with several destinations). |
| `updated_at` | timestamptz | Last status change. |
| `origin_id` | bigint | Routed from another interface (`source: internal`): the `id` of the message there. NULL for messages received from outside. Not indexed. |
| `tier` | smallint | The retention tier the row is stored in, in days (7, 30, 90, 180, 365): its interface's retention when it was stored, until a retention change moves it. 365 for rows stored by engines that don't know tiers. |

Indexes: primary key `(id, received_at, tier)` (a partitioned table's key must include its partition columns); `messages_pending (interface, partition_key, id) WHERE status = 0`; `messages_by_interface (interface, id)` (the message browser, newest first); `messages_quarantined (interface, id) WHERE status = 3`.

## `deliveries`

One row per queued message and destination: each destination's work queue and its status. Inserted in the same statement as the message. **Partitioned** like `messages` (`deliveries_t30_20261008`, same tier and bounds).

| column | type | |
|---|---|---|
| `message_id` | bigint | `messages.id`. |
| `received_at` | timestamptz | `messages.received_at` (finds the partition). |
| `destination_id` | integer | `destinations.id`. |
| `partition_key` | integer | The message's lane (copied, so the queue index needs no join). |
| `status` | smallint | This destination's status. |
| `attempts` | integer | Failed attempts so far. |
| `last_error` | text | Last failure or the reason it was filtered/quarantined. |
| `output` | bytea | Exactly what was sent, only for destinations with `save_output: true`. |
| `updated_at` | timestamptz | Last change. |
| `tier` | smallint | Always its message's tier (a retention change moves both in one statement). |

Indexes: primary key `(message_id, received_at, destination_id, tier)`; `deliveries_pending (destination_id, partition_key, message_id) WHERE status = 0`, which is each destination's queue in arrival order; `deliveries_quarantined (destination_id, message_id) WHERE status = 3` (quarantine counts without reading the table).

## `destinations`

Every destination the engine has been configured with. Deliveries refer to it by `id` (a small integer instead of repeating names in every row).

| column | type | |
|---|---|---|
| `id` | integer | Identity. |
| `interface` | text | The interface's name. |
| `name` | text | The destination's name (`default` for a single `destination:`). |

Indexes: primary key `id`; unique `(interface, name)`.

## `dedupe_keys`

Keys of recently seen messages, for interfaces with `dedupe:`. Claimed in the same transaction as the message insert. Old keys are deleted hourly.

| column | type | |
|---|---|---|
| `interface` | text | The interface's name. |
| `key` | text | The message's key values, joined. |
| `seen_at` | timestamptz | Last time it was seen (a repeat outside the window renews it). |

Indexes: primary key `(interface, key)`; `seen_at` (for the hourly cleanup).

## `source_state`

What sources remember between runs, always written in the same transaction as the messages it accounts for: file progress (`file:<name>`: size, time, byte offset, records, done) and schedules (`schedule`: anchor, previous run).

| column | type | |
|---|---|---|
| `interface` | text | The interface's name. |
| `key` | text | What it is (`file:lab.csv`, `schedule`). |
| `value` | jsonb | The state. |
| `updated_at` | timestamptz | Last change. |

Index: primary key `(interface, key)`.

## `api_tokens`

REST API tokens (`ductus token create`, or `POST /api/v1/tokens`). Only a hash of each token is kept; the token itself is shown once. Revoked tokens stay, for the record.

| column | type | |
|---|---|---|
| `id` | integer | Identity. |
| `name` | text | Who or what uses it (`ci`, `monitoring`). One live token per name. |
| `role` | smallint | `0` viewer (reads), `1` operator (also drain and connection checks), `2` admin (also tokens). |
| `hash` | bytea | SHA-256 of the token (tokens are 32 random bytes, so a slow password hash adds nothing). |
| `created_at` | timestamptz | When it was made. |
| `last_used_at` | timestamptz | Last use, to the minute (written at most once a minute per token). |
| `revoked_at` | timestamptz | When it was revoked; it stops working at once. |

Indexes: primary key `id`; unique `hash`; `api_tokens_live_name (name) WHERE revoked_at IS NULL` (unique).

## `retention`

Each interface's retention as the engine applied it, and the change in progress. Changes run one at a time per interface: a different value configured meanwhile waits in `waiting`, and so does a shorter one (it deletes messages) until someone confirms it (`ductus retention confirm`).

| column | type | |
|---|---|---|
| `interface` | text | The interface's name. |
| `days` | smallint | The tier its new messages go to (the target of a running change). |
| `changing_from` | smallint | Set while a change runs: the tier it's changing from. The interface's rows in other tiers are being moved into `days`, or deleted when they're past its window, in small batches. |
| `rows_left` | bigint | Rows still to move or delete (counted when the change started, then counted down). |
| `waiting` | smallint | A configured value not applied yet. |
| `confirmed` | smallint | A shorter `waiting` value someone confirmed. |
| `changed_at` | timestamptz | When the current value, or the running change, started. |
| `updated_at` | timestamptz | Last change. |

Index: primary key `interface`.

## Functions

- `ductus_tiers()`: the tiers, `{7,30,90,180,365}`. `ductus_tier_unit(tier)`: the period one partition of a tier covers (`day`, `week`, `month`).
- `ductus_create_tier_partition(table, tier, day)`: the partition of `messages` or `deliveries` holding that tier's rows of that day, created if missing.
- `ductus_ensure_tier_partitions(table, tier, days_ahead)`, `ductus_ensure_delivery_partitions(tier)`: one tier's missing partitions; the engine runs them hourly, one table and tier per transaction, with a short lock timeout (see architecture.md, Storage).
- `ductus_ensure_message_partitions(days_ahead)`, `ductus_ensure_delivery_partitions()`, `ductus_ensure_partitions(days_ahead)`: every tier at once, shortest tier first: what older engines call. Removed when the migrations are squashed.
