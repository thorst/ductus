# Database schema

What Ductus keeps in PostgreSQL, for transparency (and for DBAs tuning the server). **This is not an API.** Tables and columns can change in any release; read and change data through the REST API (`/api/v1`) and the `ductus` commands, which keep working across versions. Queries written against these tables may break on upgrade.

Changes only ever add (migrations in `crates/ductus-store/migrations`, see [architecture.md](architecture.md) invariant 4), so an older engine still runs on a newer schema: that's what makes rollback safe. A test checks that every table and column is described here.

Statuses (`status` columns): `0` pending, `1` sent, `2` filtered, `3` quarantined, `4` duplicate (messages only).

## `messages`

Every message received, as received (the original is never changed). **Partitioned by day** on `received_at` (`messages_YYYYMMDD`), created 3 days ahead; retention will drop whole days.

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

Indexes: primary key `(id, received_at)`; `messages_pending (interface, partition_key, id) WHERE status = 0`; `messages_by_interface (interface, id)` (the message browser, newest first); `messages_quarantined (interface, id) WHERE status = 3`.

## `deliveries`

One row per queued message and destination: each destination's work queue and its status. Inserted in the same statement as the message. **Partitioned by day** like `messages` (`deliveries_YYYYMMDD`, same bounds).

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

Indexes: primary key `(message_id, received_at, destination_id)`; `deliveries_pending (destination_id, partition_key, message_id) WHERE status = 0`, which is each destination's queue in arrival order; `deliveries_quarantined (destination_id, message_id) WHERE status = 3` (quarantine counts without reading the table).

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

## Functions

- `ductus_ensure_message_partitions(days_ahead)`, `ductus_ensure_delivery_partitions()`: create missing day partitions; the engine runs them hourly as two statements with a short lock timeout (see architecture.md, Storage).
- `ductus_ensure_partitions(days_ahead)`: the older single-transaction version, kept for older engines; removed when the migrations are squashed.
