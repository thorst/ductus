# Database schema

What Ductus keeps in PostgreSQL, for transparency (and for DBAs tuning the server). **This is not an API.** Tables and columns can change in any release; read and change data through the REST API (`/api/v1`) and the `ductus` commands, which keep working across versions. Queries written against these tables may break on upgrade.

Changes only ever add (migrations in `crates/ductus-store/migrations`, see [architecture.md](architecture.md) invariant 4), so an older engine still runs on a newer schema: that's what makes rollback safe. A test checks that every table and column is described here.

Statuses (`status` columns): `0` pending, `1` sent, `2` filtered, `3` quarantined, `4` duplicate (messages only).

## `messages`

Every message received, as received (the original is never changed). **Partitioned by retention tier, then by time** (migration 0010): `messages_t21` holds the 3-week tier (21 days), and its partitions cover a period each, named by their first day (`messages_t21_20261005`): a day for the 1w tier, a week (from Monday, midnight UTC) for 3w and 7w, about 24 partitions in all. They're created 3 days ahead; a partition is dropped once its whole period is older than its tier's days (see `retention`).

**The held area** (migration 0014) is tier `0`: `messages_held`, partitioned by month (`messages_held_202609`, bounds at midnight UTC). When a partition expires with messages someone still needs (pending, or quarantined for an interface that keeps them), those move here with their deliveries, and the partition drops. A month that's over is dropped once nothing in it is unfinished. The held tables are created when first needed.

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
| `tier` | smallint | The retention tier the row is stored in, in days (7, 21, 49: 1w, 3w, 7w): its interface's retention when it was stored, until a retention change moves it. 49 for rows stored by engines that don't know tiers. 0: in the held area. |

Indexes: primary key `(id, received_at, tier)` (a partitioned table's key must include its partition columns); `messages_pending (interface, partition_key, id) WHERE status = 0`; `messages_by_interface (interface, id)` (the message browser, newest first); `messages_quarantined (interface, id) WHERE status = 3`.

## `deliveries`

One row per queued message and destination: each destination's work queue and its status. Inserted in the same statement as the message. **Partitioned** like `messages` (`deliveries_t21_20261005`, same tier and bounds).

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
| `tier` | smallint | Always its message's tier (a retention change, or the move to the held area, moves both in one statement). |

Indexes: primary key `(message_id, received_at, destination_id, tier)`; `deliveries_queue (destination_id, message_id, partition_key) WHERE status = 0`, which is each destination's queue in arrival order (the lane is checked inside the index; migration 0011 replaced `deliveries_pending`, which had the lane second); `deliveries_quarantined (destination_id, message_id) WHERE status = 3` (quarantine counts without reading the table).

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

## `api_sessions`

Web UI sessions: a browser signs in with a token (`POST /api/v1/session`) and gets a cookie instead of keeping the token. A session acts with its token's role and ends when the token is revoked, when it's been idle for 12 hours, or 7 days after sign-in. Expired sessions are deleted when the next one is made.

| column | type | |
|---|---|---|
| `id` | integer | Identity. |
| `hash` | bytea | SHA-256 of the cookie's value (32 random bytes). |
| `token_id` | integer | The token it signed in with (`api_tokens.id`). |
| `created_at` | timestamptz | Sign-in. |
| `last_seen_at` | timestamptz | Last use, to the minute (written at most once a minute). |
| `expires_at` | timestamptz | 7 days after sign-in, whatever its use. |

Indexes: primary key `id`; unique `hash`; `api_sessions_expires (expires_at)`.

## `message_actions`

What people did to messages through the API (`POST /api/v1/messages/{id}/resend`, `/messages/resend`, `/messages/{id}/edit`): one row per message and destination acted on, with the delivery as it was before, so nothing an action changes is lost. A **resend** puts the delivery back to pending in place (same message, same id); an **edit** queues a corrected copy (a new message) and marks the original's quarantined delivery to that destination filtered ("replaced by edited copy N"). Not dropped with the messages' partitions: it's the record of who did what (step 8's audit log builds on it; no purge yet).

| column | type | |
|---|---|---|
| `id` | bigint | Identity. |
| `at` | timestamptz | When. |
| `action` | text | `resend` or `edit`. |
| `token_id` | integer | The API token used (`api_tokens.id`). |
| `actor` | text | The token's name at the time (names can be reused once a token is revoked). |
| `reason` | text | Why, if given. |
| `message_id` | bigint | The message acted on (for an edit: the original). |
| `received_at` | timestamptz | Its `received_at` (with `message_id`, finds it in its partition while it's kept). |
| `interface` | text | Its interface. |
| `destination_id` | integer | The destination (`destinations.id`). |
| `previous_status` | smallint | The delivery's status before (as `deliveries.status`); none if the message had no delivery for that destination. |
| `previous_attempts` | integer | Its attempts before (a resend starts them over). |
| `previous_error` | text | Its `last_error` before. |
| `previous_output` | bytea | What was sent before, for destinations with `save_output: true` (sending again replaces it). |
| `copy_id` | bigint | `edit`: the copy's message id. |

Indexes: primary key `id`; `message_actions_message (message_id)`; `message_actions_copy (copy_id) WHERE copy_id IS NOT NULL`.

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
| `purge_quarantined` | boolean | `quarantined: purge`: its quarantined messages go with their expired partition instead of moving to the held area (`false`, the default: kept until resolved). |

Index: primary key `interface`.

## `interface_activity`

What monitoring must remember across a restart or a failover: when each interface last received something. Written every few seconds by one task (only the times that changed; a time never moves back).

| column | type | |
|---|---|---|
| `interface` | text | The interface's name. |
| `watched_since` | timestamptz | When an engine first ran the interface: `active_by` deadlines before it don't count. |
| `last_received` | timestamptz | When a message (or a duplicate) last arrived. |
| `updated_at` | timestamptz | Last change. |

Index: primary key `interface`.

## `destination_activity`

Each destination's last send and failure (written like `interface_activity`), and its pause (written at once).

| column | type | |
|---|---|---|
| `destination_id` | integer | `destinations.id`. |
| `last_sent` | timestamptz | When a message was last delivered (or routed). |
| `last_error` | text | The last failure: a retry's error or a quarantine's reason. |
| `last_error_at` | timestamptz | When. |
| `paused_at` | timestamptz | Set while the destination is paused on purpose (`ductus pause`): nothing is sent or retried. |
| `paused_by` | text | The API token that paused it. |
| `pause_reason` | text | Why, once someone says (often added after the pause). |
| `reason_by` | text | The token that gave the reason. |
| `reason_at` | timestamptz | When. |
| `baseline_inflow` | real[] | What's normal, per hour of the week (168 values, Monday 00:00 local first; NULL for hours not watched yet): messages arriving per minute. Learned at the end of each hour the engine watched for 15 minutes or more (migration 0013). |
| `baseline_send_ms` | real[] | The same for a successful send's mean time, in ms. |
| `updated_at` | timestamptz | Last change. |

Index: primary key `destination_id`.

## `docs_snapshot`

The project's documentation as the company wrote it, for SQL reports and BI tools ("which interfaces have no signed BAA": `SELECT name FROM docs_snapshot WHERE kind = 'interface' AND docs->'custom'->>'baa_signed' = 'no'`). One row per interface and per resources.yaml entry. The YAML in Git is the source of truth: every engine start makes the table match the project (changed rows are rewritten, entries the project no longer has are removed), so it is as current as the last start. Versioned history comes with versioned releases (v0.6).

| column | type | |
|---|---|---|
| `kind` | text | `interface` or `resource`. |
| `name` | text | The interface's name, or the resource as `api empi`, `database patient_db`, `sftp server lab`. |
| `docs` | jsonb | An interface: `{"description", "owner", "custom": {...}, "destinations": {"<name>": {"custom": {...}}}}`; a resource: `{"custom": {...}}`. `custom` uses the company's field names from `docs-schema.yaml`; absent values are absent. |
| `updated_at` | timestamptz | When the row last changed. |

Indexes: primary key `(kind, name)`; `docs_snapshot_docs` (GIN on `docs`).

## Functions

- `ductus_tiers()`: the tiers in days, `{7,21,49}`. `ductus_tier_unit(tier)`: the period one partition of a tier covers (`day` for 7, else `week`).
- `ductus_create_tier_partition(table, tier, day)`: the partition of `messages` or `deliveries` holding that tier's rows of that day, created if missing.
- `ductus_create_held_partition(table, day)`: the held area's partition of `messages` or `deliveries` for that day's month (and the held table itself), created if missing.
- `ductus_ensure_tier_partitions(table, tier, days_ahead)`, `ductus_ensure_delivery_partitions(tier)`: one tier's missing partitions; the engine runs them hourly, one table and tier per transaction, with a short lock timeout (see architecture.md, Storage).
- `ductus_ensure_message_partitions(days_ahead)`, `ductus_ensure_delivery_partitions()`, `ductus_ensure_partitions(days_ahead)`: every tier at once, shortest tier first: what older engines call. Removed when the migrations are squashed.
