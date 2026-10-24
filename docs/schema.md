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
| `user_id` | integer | The user it belongs to (`users.id`); none for a service token made by an admin. A user's token stops working when the user is disabled and never goes above the user's role or permissions. |
| `permissions` | text[] | Extra permissions (`payloads`, `audit`); admins have all. |

Indexes: primary key `id`; unique `hash`; `api_tokens_live_name (name) WHERE revoked_at IS NULL` (unique); `api_tokens_user (user_id)`.

## `users`

People who sign in (`ductus user create`, or `POST /api/v1/users`). Passwords are argon2id hashes; no password is ever stored or logged. Accounts are disabled, never deleted: the audit trail names them.

| column | type | |
|---|---|---|
| `id` | integer | Identity. |
| `username` | text | Sign-in name; not case sensitive (unique on `lower(username)`). |
| `display_name` | text | For people reading the UI. |
| `email` | text | Where alerts for this person go. |
| `role` | smallint | `0` viewer, `1` operator, `2` admin (as tokens). |
| `permissions` | text[] | `payloads` (may read message payloads), `audit` (may read the audit log). Admins have both. |
| `source` | text | Where the password is checked: `local`, or `ldap` (a directory; no hash kept). |
| `password_hash` | text | argon2id, a self-describing string; null when the password lives elsewhere. |
| `must_change_password` | boolean | Set when an admin made or reset the password: the next sign-in can do nothing but change it. |
| `disabled_at` | timestamptz | When it was disabled: sessions and tokens stop at once. |
| `failed_logins` | integer | Wrong passwords counted within the lockout window. |
| `last_failed_at` | timestamptz | The latest wrong password (starts a new window when the last one is over). |
| `locked_until` | timestamptz | Past the limit, the account is refused until then. |
| `created_at` | timestamptz | |
| `last_sign_in_at` | timestamptz | |
| `directory_checked_at` | timestamptz | `ldap` accounts: when the directory was last asked about them (each sign-in, and every `auth.ldap.refresh` while they have a session or a token). |
| `directory_refused_at` | timestamptz | `ldap` accounts: when the directory said no (in no Ductus group, disabled or deleted there). The account can't act (sessions ended, tokens stopped), like a disabled one, until a directory check says yes again; separate from `disabled_at`, which is an admin's decision here. |

Directory accounts (`source = 'ldap'`) are made at their first sign-in; their role, permissions, display name and email are the directory's, rewritten at every sign-in and refresh (migration 0022 added the two `directory_` columns).

Indexes: primary key `id`; unique `users_username (lower(username))`.

## `user_settings`

A user's own choices, so they follow them to any machine. Today: the theme and the custom colors.

| column | type | |
|---|---|---|
| `user_id` | integer | `users.id`. Primary key. |
| `theme` | text | `system`, `light`, `dark`, `contrast` or `custom`. |
| `colors` | jsonb | The five custom colors and the values worked out from them, as the UI saves them. |
| `updated_at` | timestamptz | |

## `api_sessions`

Web UI sessions: a browser signs in with a username and password, or a token (`POST /api/v1/session`), and gets a cookie instead of keeping either. A session acts with its user's or token's role and ends when the user is disabled or the token revoked, when it's been idle too long (12 hours unless `auth.session` says otherwise), or at most 7 days after sign-in. Expired sessions are deleted when the next one is made.

| column | type | |
|---|---|---|
| `id` | integer | Identity. |
| `hash` | bytea | SHA-256 of the cookie's value (32 random bytes). |
| `token_id` | integer | The token it signed in with (`api_tokens.id`); none for a user's session. |
| `user_id` | integer | The user it signed in as (`users.id`); none for a token's session. Exactly one of the two is set. |
| `created_at` | timestamptz | Sign-in. |
| `last_seen_at` | timestamptz | Last use, to the minute (written at most once a minute). |
| `expires_at` | timestamptz | When it ends at the latest, whatever its use. |

Indexes: primary key `id`; unique `hash`; `api_sessions_expires (expires_at)`; `api_sessions_user (user_id)`.

## `message_actions`

What people did to messages through the API (`POST /api/v1/messages/{id}/resend`, `/messages/resend`, `/messages/{id}/edit`): one row per message and destination acted on, with the delivery as it was before, so nothing an action changes is lost. A **resend** puts the delivery back to pending in place (same message, same id); an **edit** queues a corrected copy (a new message) and marks the original's quarantined delivery to that destination filtered ("replaced by edited copy N"). Not dropped with the messages' partitions: it's the record of who did what (step 8's audit log builds on it; no purge yet).

| column | type | |
|---|---|---|
| `id` | bigint | Identity. |
| `at` | timestamptz | When. |
| `action` | text | `resend`, `edit` or `dismiss` (a quarantined delivery closed without sending: it became `filtered`, "dismissed by NAME: reason"). |
| `token_id` | integer | The API token used (`api_tokens.id`). |
| `user_id` | integer | The person who acted (`users.id`), when it was one (their token's owner counts). |
| `actor` | text | The user's or token's name at the time (names can be reused). |
| `reason` | text | Why, if given. |
| `message_id` | bigint | The message acted on (for an edit: the original). |
| `received_at` | timestamptz | Its `received_at` (with `message_id`, finds it in its partition while it's kept). |
| `interface` | text | Its interface. |
| `destination_id` | integer | The destination (`destinations.id`). |
| `previous_status` | smallint | The delivery's status before (as `deliveries.status`); none if the message had no delivery for that destination. |
| `previous_attempts` | integer | Its attempts before (a resend starts them over). |
| `previous_error` | text | Its `last_error` before. |
| `previous_output` | bytea | What was sent before, for destinations with `save_output: true` (sending again replaces it). A copy of a payload, so it is set to NULL once the action is older than the interface's message retention (the row stays). |
| `copy_id` | bigint | `edit`: the copy's message id. |
| `audit_id` | bigint | The `audit_log` row of the request that made this action (one request, e.g. a bulk resend, makes one audit row and one action per delivery). No foreign key: both are trimmed after `audit.keep`. |

Indexes: primary key `id`; `message_actions_message (message_id)`; `message_actions_copy (copy_id) WHERE copy_id IS NOT NULL`; `message_actions_audit (audit_id) WHERE audit_id IS NOT NULL`; `message_actions_outputs (at) WHERE previous_output IS NOT NULL` (finds the copies to clear). Rows older than `audit.keep` (default 6 years) are deleted hourly in batches of 5,000.

## `audit_log`

Who did what, to what, from where, and how it ended. Written in the transaction of the change itself where there is one (a resend, a dismissal, an account change), and before the action where there isn't (a pause, a drain; the action is refused if the row can't be written). Payload reads are written before the payload is returned. Not tied to users or tokens by a foreign key: it must say who acted long after the account is gone. Rows older than `audit.keep` are deleted hourly in batches of 5,000.

| column | type | |
|---|---|---|
| `id` | bigint | Identity. |
| `at` | timestamptz | When. |
| `actor_kind` | text | `user`, `token`, `system` (the engine, or someone at the server's command line) or `anonymous` (a sign-in as a name nobody has; what they typed isn't kept, it may be a password). |
| `actor_id` | integer | `users.id` or `api_tokens.id`, by `actor_kind`. |
| `user_id` | integer | The person behind it: the user, or the owner of a user's token. What `GET /me/activity` reads. |
| `actor_name` | text | The user's or token's name at the time (a snapshot). |
| `via` | text | `session` (the web UI), `token`, `cli`, `engine`; none for rows made from `message_actions` when this table was added. |
| `ip` | inet | The address the connection came from (a proxy's address behind one). |
| `action` | text | `session.sign_in`, `session.failed`, `session.sign_out`, `password.change`, `password.reset`, `user.create` (also a directory account's first sign-in, by `directory`), `user.update`, `user.disable`, `user.unlock`, `user.directory_change` (the directory changed an account's role, permissions, name or email, or let it back in), `user.directory_refused` (the directory said no to an account: its sessions ended), `token.create`, `token.revoke`, `message.payload.read`, `message.release` (a resend that put back only quarantined deliveries, whether or not it named their destinations), `message.replay` (one that sent again a delivery that had finished as sent or filtered), `message.edit`, `message.dismiss`, `destination.pause`, `destination.resume`, `engine.drain`, `engine.resume`, `retention.change`, `retention.confirm`, `doctor.run`, `audit.export`, `alert.ack`, `alert.silence`, `alert.unsilence`, `alert.test` (a test notification to a channel), `engine.start`, `engine.stop`, `engine.refused` (a start refused: another live engine runs some of its interfaces, or its lanes would strand waiting messages; `detail.reason` is `owned` or `lanes`), `interface.stopped`, `interface.partitions_raised` (an interface started with more lanes while messages waited: a key's new messages may pass its older ones until those are sent). |
| `subject` | text | What it was done to: a username, a token name, `interface/destination`. |
| `interface` | text | The interface it concerns. |
| `message_id` | bigint | The message, when the request was about exactly one. |
| `outcome` | text | `ok`, `denied` or `failed`. |
| `detail` | jsonb | More, by action: the reason, the messages and destinations of a bulk request, the version and previous run for `engine.start`. Never a password, token or payload. |

Indexes: primary key `id`; `audit_log_at (at)`; `audit_log_actor (actor_kind, actor_id, at)`; `audit_log_user (user_id, at)`; `audit_log_action (action, at)`; `audit_log_interface (interface, at)`; `audit_log_message (message_id)`.

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

## `engines`

Every run of an engine on this database (migration 0023). While it runs, an engine holds a session advisory lock with `lock_key` on a connection of its own; that is what "alive" means here (`pg_locks`), so a crash or power loss needs no timeout to wait out (the server frees the lock with the session; the lock connection asks it to notice a vanished client within ~25 s). A refused start leaves no row. Rows of engines that started more than `audit.keep` ago, own nothing and aren't alive are deleted by the hourly audit job.

| column | type | |
|---|---|---|
| `instance` | text | The run's name, as `audit_log` `engine.start`/`engine.stop` carry it in `detail.instance`. |
| `lock_key` | bigint | Its advisory lock (positive, below 2^62: `pg_locks` shows it as `classid` × 2^32 + `objid`). |
| `host` | text | The server it runs on. |
| `pid` | integer | Its process id there. |
| `version` | text | The engine's version. |
| `environment` | text | `--env`. |
| `started_at` | timestamptz | When it started. |
| `stopped_at` | timestamptz | Set by a clean stop. NULL and not alive: it crashed. |

Indexes: primary key `instance`; unique `lock_key`.

## `interface_owners`

Which engine runs each interface's workers, file pickup and schedule: one engine per interface across every engine on the database. An engine claims all of its (queued) interfaces at start, before changing anything for them, all or none; a row whose engine isn't alive any more is taken over. Respond-mode HTTP interfaces aren't claimed (no queue). A clean stop deletes its rows.

| column | type | |
|---|---|---|
| `interface` | text | The interface's name. |
| `instance` | text | `engines.instance` (deleted with it). |
| `since` | timestamptz | When it was claimed. |

Indexes: primary key `interface`; `interface_owners_instance (instance)`.

## `interface_applied`

The lanes each interface last ran with: `ordering.partitions`, or 1 without `ordering.key`. At start, an interface with fewer lanes than recorded (or none recorded) has its waiting deliveries checked: any in lanes it no longer has refuse the start (no worker would read them). More lanes with messages waiting only warn (`interface.partitions_raised`).

| column | type | |
|---|---|---|
| `interface` | text | The interface's name. |
| `lanes` | integer | Its lanes. |
| `updated_at` | timestamptz | When it last changed. |

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

## `alert_state`

One row per thing that can alert: each interface (`iface:<interface>`), each destination (`dest:<interface>/<destination>`) and each certificate the engine connects to (`cert:<resource>:<host>:<port>`) or presents (`cert:<name>:<engine's host>:0`), made when an engine with `alerts:` in ductus.yaml first runs it (so it can be silenced before anything fires). Changed by compare-and-set on `rev`: two engines on one database judge the same state and only one records each change.

| column | type | |
|---|---|---|
| `id` | integer | Identity; the API's `/alerts/{id}`. |
| `key` | text | `iface:<interface>`, `dest:<interface>/<destination>` or `cert:<resource>:<host>:<port>` (unique). |
| `interface` | text | The interface; for a certificate, its resource (as in `certificate_status`). |
| `destination` | text | The destination; none for the interface's own alert; `host:port` for a certificate. |
| `state` | text | `ok`, or what is wrong: `error`, `stalled`, `behind` (a destination), `stopped`, `silent`, `missed` (an interface), `certificate` (expires within `alerts.certificates.warn_days`, or has expired). |
| `severity` | text | `warning` or `critical` (from the first rule naming the state), while open. |
| `since` | timestamptz | Bad since (when it fired). |
| `ok_since` | timestamptz | Fine again since then, not yet for `alerts.settle`: bad again before that reopens the same alert. |
| `last_notified_at` | timestamptz | When people were last told, or will be once quiet hours end; reminders count from it. |
| `notify_count` | integer | Notifications sent since it fired. |
| `summary` | text | The health view's words, as of the last check. |
| `ack_by` | text | Who acknowledged it (no reminders until it escalates or recovers). |
| `ack_at` | timestamptz | When. |
| `silenced_until` | timestamptz | Nothing is sent about it until then. |
| `silenced_by` | text | Who silenced it. |
| `silence_reason` | text | Why. |
| `rev` | bigint | Each change adds one (compare-and-set). |
| `updated_at` | timestamptz | Last change. |
| `stage` | integer | A certificate alert's step: the warn day (30, 14, 7 by default) people were last told about, `0` once expired (migration 0021). None for other alerts. |

Indexes: primary key `id`; unique `key`.

## `alert_events`

What happened to each alert: `fired`, `escalated`, `reminded`, `recovered` (the engine) and `acknowledged`, `silenced`, `unsilenced` (people, also in `audit_log`). Rows older than `alerts.keep` (default a year) are deleted hourly with their notifications.

| column | type | |
|---|---|---|
| `id` | bigint | Identity. |
| `at` | timestamptz | When. |
| `alert_id` | integer | `alert_state.id` (no foreign key). |
| `key` | text | The alert's key. |
| `kind` | text | See above. |
| `state` | text | The state then; for `recovered`, what it recovered from. |
| `severity` | text | The severity then. |
| `since` | timestamptz | When the alert turned bad, for the words of a notification sent later. |
| `summary` | text | The health view's words then. |
| `by_name` | text | Who acknowledged or silenced it. |
| `detail` | jsonb | For a silence: `until` and `reason`. |

Indexes: primary key `id`; `alert_events_alert (alert_id, id)`; `alert_events_at (at)`.

## `alert_deliveries`

The outbox: one row per notification (an event to a channel), stored with the event before anything is sent. Senders claim due rows with `FOR UPDATE SKIP LOCKED` and a two-minute lease, so two engines send each once; a crash between sending and marking it sent sends it again after the lease (at least once).

| column | type | |
|---|---|---|
| `id` | bigint | Identity. |
| `event_id` | bigint | `alert_events.id` (deleted with it). |
| `channel` | text | A channel's name; `<name>/email` and `<name>/webhook` for a channel with both; `email:<address>` for an owner. |
| `status` | text | `pending`, `sent`, `failed` (given up after 10 tries, about six hours), `cancelled` (it recovered before quiet hours ended). |
| `attempts` | integer | Tries so far. |
| `last_error` | text | Why the last try failed. |
| `next_try_at` | timestamptz | Due then (after quiet hours, or a back-off of 30 s doubling to an hour); while claimed, the end of the lease. |
| `sent_at` | timestamptz | When it went out. |

Indexes: primary key `id`; `alert_deliveries_due (next_try_at) WHERE status = 'pending'`; `alert_deliveries_event (event_id)`.

## `certificate_status`

The certificate each TLS server the engine connects to presented at its last check (migration 0021): HTTPS APIs and destinations, the alerts' mail server, webhooks and heartbeat. Written by the engine's scheduled checks (`doctor.schedule`, every 6 hours by default) and by checks someone asks for (`POST /api/v1/doctor`). Read even when this server doesn't trust it (a private CA). A check that couldn't read a certificate sets `error` and `checked_at` and keeps the dates from the check before.

Our own certificates are rows too (migration 0024, `side = 'ours'`): each `certificates:` pair the engine presents, to senders on a TLS listener or as a client certificate. They are read from what the engine loaded (no network), written at its start and again only when a renewal is picked up or a renewed pair can't be used.

| column | type | |
|---|---|---|
| `side` | text | `remote` (a server the engine connects to) or `ours` (a pair it presents; migration 0024). Older engines' rows are `remote`. |
| `resource` | text | `empi` (resources.yaml), `alerts.smtp`, `alerts.<channel>` (a webhook), `alerts.heartbeat`, or `<interface>/<destination>` for a destination with a URL of its own; ours: the `certificates:` entry's name. |
| `host` | text | The server's name as configured; ours: the engine's server (its host name). |
| `port` | integer | Its port; ours: 0 (one pair may be served on several ports). |
| `subject` | text | The server certificate's common name (or organisation). |
| `issuer` | text | Who issued it. |
| `fingerprint` | text | SHA-256 of the server certificate, hex: changes when it's renewed or replaced. |
| `not_before` | timestamptz | Valid from. |
| `not_after` | timestamptz | The server certificate's expiry. |
| `chain_min_not_after` | timestamptz | The earliest expiry in the chain the server presents: what alerts count (an intermediate that ends breaks clients as surely). |
| `chain_min_subject` | text | Whose that is, when it isn't the server certificate's. |
| `trusted` | boolean | Whether this server trusts it (the system's trusted certificates); none for ours (whether senders do isn't known here). |
| `error` | text | Why the last check failed (couldn't connect, not trusted, expired); none when it passed. Ours: why renewed files can't be used (the pair loaded before is still presented). |
| `checked_at` | timestamptz | The last check; ours: when the engine last read the pair (its start, or a renewal). |
| `read_at` | timestamptz | When the certificate in the row was read: the last check, unless that one couldn't read it (then `error` says why and the dates are older). |

Index: primary key `(resource, host, port)`.

## `doctor_runs`

Each connection check the engine ran on its schedule or was asked for, with its whole report (migration 0021). The last 30 are kept.

| column | type | |
|---|---|---|
| `id` | bigint | Identity. |
| `at` | timestamptz | When it ran. |
| `mode` | text | `certificates` (DNS, TCP, TLS handshake: nothing logs in) or `full` (`ductus doctor`'s checks, logins included). |
| `started_by` | text | `schedule`, or who asked (a user or token name). |
| `seconds` | double precision | How long it took. |
| `checks` | integer | Connections checked. |
| `failed` | integer | Of those, failed. |
| `report` | jsonb | The report, as `POST /api/v1/doctor` returns it. |

Index: primary key `id`.

## Functions

- `ductus_tiers()`: the tiers in days, `{7,21,49}`. `ductus_tier_unit(tier)`: the period one partition of a tier covers (`day` for 7, else `week`).
- `ductus_create_tier_partition(table, tier, day)`: the partition of `messages` or `deliveries` holding that tier's rows of that day, created if missing.
- `ductus_create_held_partition(table, day)`: the held area's partition of `messages` or `deliveries` for that day's month (and the held table itself), created if missing.
- `ductus_ensure_tier_partitions(table, tier, days_ahead)`, `ductus_ensure_delivery_partitions(tier)`: one tier's missing partitions; the engine runs them hourly, one table and tier per transaction, with a short lock timeout (see architecture.md, Storage).
- `ductus_ensure_message_partitions(days_ahead)`, `ductus_ensure_delivery_partitions()`, `ductus_ensure_partitions(days_ahead)`: every tier at once, shortest tier first: what older engines call. Removed when the migrations are squashed.
