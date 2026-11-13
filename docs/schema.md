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
| `partition_key` | integer | The lane, from the ordering key in the interface's newest lane generation (`lane_generations`); a resend moves it to the newest generation. |
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
| `partition_key` | integer | The message's lane (copied, so the queue index needs no join); a resend puts the delivery back in its key's lane in the newest lane generation. |
| `status` | smallint | This destination's status. |
| `attempts` | integer | Failed attempts so far. |
| `last_error` | text | Last failure or the reason it was filtered/quarantined. |
| `output` | bytea | Exactly what was sent, only for destinations with `save_output: true`. |
| `updated_at` | timestamptz | Last change. |
| `tier` | smallint | Always its message's tier (a retention change, or the move to the held area, moves both in one statement). |
| `code_version` | integer | The code version that decided it (`code_versions.id`; migration 0027, 0.7.3.2): set with its final status by the engine's workers, sent, filtered or quarantined, also when the receiver refused it (the code decided to send). Null while pending (a resend clears it, keeping it in `message_actions.previous_code_version`), when a person decided it (dismissed, replaced by an edited copy), when the engine did without running code (a destination removed from interface.yaml, an older engine's status taken over at start), and on deliveries decided by engines before 0.7.3.2. No foreign key (it would cost a lookup per delivery saved; versions are never deleted). 4 bytes once set, nothing while null. Which deployment brought that version: the last `deployment_changes` row for the interface with `to_version` = this, at or before `updated_at`. |

Indexes: primary key `(message_id, received_at, destination_id, tier)`; `deliveries_queue (destination_id, message_id, partition_key) WHERE status = 0`, which is each destination's queue in arrival order (the lane is checked inside the index; migration 0011 replaced `deliveries_pending`, which had the lane second); `deliveries_quarantined (destination_id, message_id) WHERE status = 3` (quarantine counts without reading the table).

## `destinations`

Every destination the engine has been configured with. Deliveries refer to it by `id` (a small integer instead of repeating names in every row).

| column | type | |
|---|---|---|
| `id` | integer | Identity. |
| `interface` | text | The interface's name. |
| `name` | text | The destination's name, as under `destinations:` (`default` on databases that ran an interface with the singular `destination:` before 0.7.4). |

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

| `directory_id` | text | `ldap` accounts: the person's id in the directory, `<attribute>:<value>` (`entryUUID:…`, or `objectGUID:hex:…` for Active Directory's 16 bytes; `auth.ldap.attributes.id`). Set at the first sign-in or refresh that reads it (null before 0.6.7.1, and with `id: none`). A refresh looks the person up by it. |

Directory accounts (`source = 'ldap'`) are made at their first sign-in; their role, permissions, display name and email are the directory's, rewritten at every sign-in and refresh (migration 0022 added the two `directory_` columns). An account is the directory's *person* once its `directory_id` is known (0.6.7.1, migration 0025): when the directory renames their sign-in name, `username` takes the new one; when it gives a departed person's sign-in name to someone new, the earlier account is renamed `<name>#<id>` (its own `id`) and refused, so its tokens stay dead, and the new person gets an account of their own.

Indexes: primary key `id`; unique `users_username (lower(username))`; unique `users_directory_id (directory_id) WHERE directory_id IS NOT NULL`.

## `user_names`

What an account was called before (0.6.7.1, migration 0025): every change of its username, display name or email (an admin's edit, the directory's at a sign-in or refresh, a name moved aside) keeps the old value here, in the same transaction as the change. Kept as long as the account. Empty old values aren't kept.

| column | type | |
|---|---|---|
| `id` | bigint | Identity. |
| `user_id` | integer | `users.id` (deleted with it). |
| `kind` | text | `username`, `email` or `display_name`. |
| `value` | text | The value it had. |
| `until` | timestamptz | When it changed. |

Indexes: primary key `id`; `user_names_value (lower(value))`; `user_names_user (user_id)`.

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
| `previous_code_version` | integer | The code version that had decided the delivery (`deliveries.code_version` before: `code_versions.id`; migration 0027, 0.7.3.2); null when no code had, or an engine before 0.7.3.2 did. |
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
| `action` | text | `session.sign_in`, `session.failed`, `session.sign_out`, `password.change`, `password.reset`, `user.create` (also a directory account's first sign-in, by `directory`), `user.update`, `user.disable`, `user.unlock`, `user.directory_change` (the directory changed an account's role, permissions, name or email, or let it back in), `user.directory_refused` (the directory said no to an account: its sessions ended), `token.create`, `token.revoke`, `message.payload.read`, `message.release` (a resend that put back only quarantined deliveries, whether or not it named their destinations), `message.replay` (one that sent again a delivery that had finished as sent or filtered), `message.edit`, `message.dismiss`, `destination.pause`, `destination.resume`, `engine.drain`, `engine.resume`, `retention.change`, `retention.confirm`, `doctor.run`, `audit.export`, `alert.ack`, `alert.silence`, `alert.unsilence`, `alert.test` (a test notification to a channel), `engine.start`, `engine.stop`, `engine.handover` (this engine took over from another: `systemctl reload`, an upgrade or a deploy; `detail.from` and `detail.to` with version, pid and instance, `detail.sending_paused_ms`), `engine.refused` (a start refused: another live engine runs some of its interfaces, `detail.reason` `owned`; before 0.7.4.4.4 also `lanes`, its lanes would strand waiting messages, and `handover_lanes`, a handover would lower lanes), `interface.stopped`, `interface.partitions_raised` (before 0.7.4.4.4: an interface started with more lanes while messages waited, so a key's new messages could pass its older ones), `interface.lanes_changed` (an interface's `ordering` changed: a lane generation added; `detail.before`, `detail.now`, `detail.key_changed`, `detail.adopted` when an older version ran it since, `detail.waiting`, `detail.first_lane`), `interface.lanes_settled` (nothing waits in its old lanes: they were deleted; `detail.lanes`, `detail.first_lane`, `detail.seconds` since the engine started), `release.store` (a release stored with `POST /releases`; `subject` its short id, `detail` its project, commit, maker and reason), `release.check` (a stored release checked against this engine's environment; `detail.environment`, `detail.ok`, `detail.problems`), `release.deploy` (a release deployed to this engine, or a deploy refused or failed: `detail.ok`, `detail.outcome`, `detail.reason`, `detail.check`, `detail.reused_check`), `release.force` (a release deployed over someone else's work the "replaces" check named: `detail.replaces`, `detail.reason`), `release.rollback` (this engine rolled back to a release it ran before, or a rollback refused or failed: `detail.ok`, `detail.outcome`, `detail.reason`, `detail.from`, `detail.undoes`), `release.download` (a stored release's file downloaded with `GET /releases/{id}/file`), `release.pull` (a release brought here from the engine this one pulls from, `DUCTUS_PROMOTE_FROM`, stored new: `detail.from`, `detail.environment`, besides `release.store`'s). |
| `subject` | text | What it was done to: a username, a token name, `interface/destination`. |
| `interface` | text | The interface it concerns. |
| `message_id` | bigint | The message, when the request was about exactly one. |
| `outcome` | text | `ok`, `denied` or `failed`. |
| `detail` | jsonb | More, by action: the reason, the messages and destinations of a bulk request, the version and previous run for `engine.start`. Never a password, token or payload. |

Indexes: primary key `id`; `audit_log_at (at)`; `audit_log_actor (actor_kind, actor_id, at)`; `audit_log_user (user_id, at)`; `audit_log_action (action, at)`; `audit_log_interface (interface, at)`; `audit_log_interface_changes (interface, at)` where the row names an interface, isn't a payload read and was allowed (migration 0028: what `ductus history` reads, without reading through an interface's payload reads); `audit_log_message (message_id)`.

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

Where each interface's lanes end (migration 0023). Before 0.7.4.4.4 the lanes it last ran with (`ordering.partitions`, or 1 without `ordering.key`), checked at start for messages in lanes it no longer had; since, one past its newest lane generation's last lane (`lane_generations`), kept for older binaries: one started on this database sees a lowering and refuses if messages wait above its own lanes. A start that finds a value other than that end knows an older binary ran the interface since, and takes its lanes in (adopted).

| column | type | |
|---|---|---|
| `interface` | text | The interface's name. |
| `lanes` | integer | One past its last lane (before 0.7.4.4.4: its lane count). |
| `updated_at` | timestamptz | When it last changed. |

Index: primary key `interface`.

## `lane_generations`

The lanes each interface's messages are stored in (migration 0033). A message's lane is worked out once, at intake: `first_lane + hash(key) % lanes` of the newest generation. A change of `ordering` (the lane count or the key) adds a generation numbered after the last one's lanes; workers read every generation listed, holding a newer lane for a destination while older lanes that may hold the same keys still have messages for it, and older generations are deleted once nothing waits in them (`interface.lanes_settled`), so the steady state is one row per interface. A start that finds none recorded, or finds `interface_applied` not at the newest generation's end, adopts the lanes anything waits in as one `mixed` generation. A handover that fails takes back the generations it added unless messages were stored in them. Read at each start and on a resend (a resent delivery goes to its key's lane in the newest generation).

| column | type | |
|---|---|---|
| `interface` | text | The interface's name. |
| `first_lane` | integer | Its lanes are `first_lane` to `first_lane + lanes - 1`; generations never overlap, and a newer one starts where the last ended. |
| `lanes` | integer | How many (`ordering.partitions`, or 1 without `ordering.key`). |
| `key` | text | `ordering.key` (`''` without one); NULL: not known (made from `interface_applied` by the migration; the next start records it without a new generation). |
| `mixed` | boolean | Adopted: its messages' lanes don't follow a key, so every newer lane waits for all of it. |
| `since` | timestamptz | When it was added. |

Index: primary key `(interface, first_lane)`.

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

The project's documentation as the company wrote it, for SQL reports and BI tools ("which interfaces have no signed BAA": `SELECT name FROM docs_snapshot WHERE kind = 'interface' AND docs->'custom'->>'baa_signed' = 'no'`). One row per interface and per resources.yaml entry. The YAML in Git is the source of truth: every engine start makes the table match the project (changed rows are rewritten, entries the project no longer has are removed), so it is as current as the last start. Since 0.7.3.3 it's saved with the start's deployment, beside the start (in the same transaction, one engine at a time), and each change is a row of `docs_history`.

| column | type | |
|---|---|---|
| `kind` | text | `interface` or `resource`. |
| `name` | text | The interface's name, or the resource as `api empi`, `database patient_db`, `sftp server lab`. |
| `docs` | jsonb | An interface: `{"description", "owner", "custom": {...}, "destinations": {"<name>": {"custom": {...}}}}`; a resource: `{"custom": {...}}`. `custom` uses the company's field names from `docs-schema.yaml`; absent values are absent. |
| `updated_at` | timestamptz | When the row last changed. |

Indexes: primary key `(kind, name)`; `docs_snapshot_docs` (GIN on `docs`).

## `docs_history`

Every change of `docs_snapshot` (migration 0028, 0.7.3.3): a row each time an entry's documentation changed, appeared or left the project, with the deployment that did it. What `ductus history` and the History tab show as "documentation changed: custom.baa_signed: no → yes". The upgrade copied the snapshot as it was then (rows without a deployment), so each entry's history starts there. Kept.

| column | type | |
|---|---|---|
| `id` | bigint | From a sequence: an entry's rows in order. |
| `deployment_id` | integer | `deployments.id` that saved it; null for the copy made at the upgrade, or a start whose deployment couldn't be recorded (set null if that deployment row is deleted). |
| `at` | timestamptz | When it was saved. |
| `kind` | text | `interface` or `resource`, as in `docs_snapshot`. |
| `name` | text | As in `docs_snapshot`. |
| `docs` | jsonb | The documentation from then on, as in `docs_snapshot`; null: the entry left the project. What it was before is the entry's previous row. |

Indexes: primary key `id`; `docs_history_entry (kind, name, id)`.

## `deployments`

One row per engine start and handover (migration 0026, 0.7.3), whether or not anything changed: what it runs, by whom, why, from which git commit. A few hundred bytes each; kept. The guide: [what runs, recorded](operate/deployments.md).

| column | type | |
|---|---|---|
| `id` | integer | From a sequence. |
| `at` | timestamptz | When it was recorded (once the engine held its interfaces). |
| `environment` | text | `DUCTUS_ENV`. |
| `instance` | text | The engine run, as in the audit log's `engine.start` and `engine.handover`. |
| `host` | text | The machine's name. |
| `version` | text | The binary's version. |
| `how` | text | `start` (someone ran it), `service` (systemd or the Windows service started it) or `handover`. |
| `by_name` | text | Who: the account that started it, the person who asked for the handover (`sudo ductus handover`: the person behind `sudo`), or `systemd (systemctl reload)`. |
| `reason` | text | Why (`ductus handover -m`); null when nobody said. |
| `git_commit` | text | The project's git commit (HEAD, in hex); null when the project isn't in a git repository, or the repository has no commit yet. |
| `uncommitted` | text[] | Files the interfaces run that aren't the commit's (changed since, or never committed), as `code_versions.files` names them; null without git, empty when everything is committed. |
| `interfaces` | integer | How many interfaces it runs. |
| `changed` | integer | How many of them run a code version they didn't run before it. |
| `interface_set` | integer | `interface_sets.id`: which interfaces it ran, changed or not (migration 0028); null for deployments recorded before 0.7.3.3 or by an older engine. |
| `release_id` | bytea | Release mode (`ductus run --releases`, migration 0031): the release it ran (`releases.id`; no foreign key, releases are kept). The release last deployed to an environment for a project is what its engines run. Null in folder mode and before 0.7.4.4; `git_commit` and `uncommitted` are then the release's. |
| `project_version` | bytea | Release mode: the release's project version (SHA-256 over the files outside every interface), which a deploy holds against the next one's. Null in folder mode. |
| `rollback` | boolean | The deployment was a rollback (`ductus deploy --rollback`, migration 0032, 0.7.4.4.3): the next rollback's default skips the release it went back from. Null otherwise. |

Indexes: primary key `id`; `deployments_at (at)`; `deployments_interface_set (interface_set, id)`; `deployments_releases (environment, id) WHERE release_id IS NOT NULL`.

## `interface_sets`

Which interfaces a deployment ran (migration 0028, 0.7.3.3), once per distinct set: a start that runs what the last one ran adds nothing. It's how `ductus history --all` shows the starts that ran an interface without changing it (`deployment_changes` names only those that changed it), and keeps engines with different projects on one database apart. Kept.

| column | type | |
|---|---|---|
| `id` | integer | From a sequence. |
| `hash` | bytea | SHA-256 of the names, sorted, each followed by a line feed. Unique. |
| `interfaces` | text[] | The interfaces' names, sorted. |

Indexes: primary key `id`; unique `hash`; `interface_sets_interfaces` (GIN on `interfaces`).

## `deployment_changes`

The interfaces a deployment changed (migration 0026): one row each, none for interfaces that run what they ran before.

| column | type | |
|---|---|---|
| `deployment_id` | integer | `deployments.id` (deleted with it). |
| `interface` | text | The interface's name. |
| `from_version` | integer | `code_versions.id` it ran before; null when it's new on this database. |
| `to_version` | integer | `code_versions.id` it runs from this deployment. |
| `warning` | text | Why this change may undo someone else's (migration 0028, 0.7.3.3; folder mode, two people deploying from their own checkouts): the version it replaced was deployed by someone else and came from a commit not in this deployment's commit's history, or ran files never committed. Null when there's nothing to say. The same text is in the log and in `ductus handover`'s answer. |

Indexes: primary key `(deployment_id, interface)`; `deployment_changes_interface (interface, deployment_id)`.

## `interface_versions`

What each interface runs now (migration 0026): its last deployment's version, read at every start to tell what changed. One row per interface ever run on this database.

| column | type | |
|---|---|---|
| `interface` | text | The interface's name. |
| `version_id` | integer | `code_versions.id`. |
| `deployment_id` | integer | `deployments.id` that version came with. |

Index: primary key `interface`.

## `code_versions`

Each distinct version of an interface's code (migration 0026): a start that changes nothing adds none. The version is a SHA-256 over the interface's name and its files' paths and hashes (its interface.yaml and every project file its code imports, shared ones too; not the SDK), so the same files give the same version on any machine: how it's computed is in [what runs, recorded](operate/deployments.md#the-code-version).

| column | type | |
|---|---|---|
| `id` | integer | From a sequence. |
| `hash` | bytea | The version (32 bytes); shown as its first 12 hex characters. Unique. |
| `interface` | text | The interface's name. |
| `files` | jsonb | `[{"path": "interfaces/adt/interface.yaml", "hash": "<hex>"}, ...]` in path order: paths relative to the project with `/`, each file's SHA-256 (its text in `code_files`). |
| `first_seen` | timestamptz | When a start first ran it. |
| `parts` | jsonb | Which of `files` each part runs (migration 0029, 0.7.4.2): `{"source": [...], "destinations": {"lab": ["interfaces/adt/lab.ts", "lib/codes.ts"], "archive": []}}`: `source` an HTTP `respond` interface's code (empty otherwise), each destination the file its function is in and everything that imports (empty: no code). What history by destination tells a code change by. Null: saved before 0.7.4.2 and not run since (the first engine that runs it again fills it in, once). |

Indexes: primary key `id`; unique `hash`; `code_versions_interface (interface)` (migration 0028: `history` reads every version an interface ran, for the files they named).

## `code_files`

Every file any code version has had, once by its content (migration 0026), so the code that ran can be read (and run again) without git: `SELECT content FROM code_files WHERE hash = decode('<hex>', 'hex')`. A start writes only files not there yet: a project's first start stores it whole (about a megabyte of text for a thousand interfaces), later ones only what changed. Postgres compresses long values itself. Kept.

| column | type | |
|---|---|---|
| `hash` | bytea | SHA-256 of `content`. |
| `content` | text | The text as hashed: without a byte order mark, lines ending in `\n`. |

Index: primary key `hash`.

Since 0.7.4.3.3 the text files of stored releases are kept here too (the same text, the same hash: a release and a start share them).

## `releases`

Each release stored on this database with `POST /releases` (migration 0030, 0.7.4.3.3): made by `ductus release`, its id recomputed from its files when it was stored. Kept (a release is what a deploy and a rollback choose from); a release's files are kept by folder, each folder's list once (`release_folders`), so a release that changed ten interfaces adds about ten folders and their changed files.

| column | type | |
|---|---|---|
| `id` | bytea | The release id (32 bytes): SHA-256 over its files' hashes and paths ([how](operate/releases.md#the-release-id)). Shown as its first 12 hex characters. |
| `project` | text | The project's folder name. |
| `manifest` | jsonb | Its `release.json` as received: every interface's code version and files, the project version, the tests' result, who made it and why. Hundreds of KB at a thousand interfaces: lists read the columns below instead. |
| `made_at` | timestamptz | When `ductus release` made it (null when the manifest's time doesn't read as one). |
| `made_by` | text | Who made it (the account that ran `ductus release`). |
| `reason` | text | Why (`ductus release -m`); null when nobody said. |
| `git_commit` | text | The commit it was made from (hex); null when made without git. |
| `interfaces` | integer | How many interfaces it carries. |
| `files` | integer | How many files it carries. |
| `bytes` | bigint | Their bytes, unpacked. |
| `ancestry` | bytea | The commit's history: 20-byte commit ids, sorted (empty without git). What the "replaces" check of a deploy (0.7.4.4) reads. 20 KB per 1,000 commits. |
| `folders` | bytea[] | Its folders (`release_folders.hash`), in path order. |
| `stored_at` | timestamptz | When it was stored here. |
| `stored_by` | text | Who stored it (a person's or a token's name, as the audit log names them). |
| `branches` | text[] | Every branch it was made on, as its files said (`git.branch`), added at each upload (migration 0032, 0.7.4.4.3): the same files made on a feature branch and again on `main` after a fast-forward merge are one release, made on both. What `DUCTUS_DEPLOY_BRANCHES` reads (any may match). Null: none said (a detached HEAD without `ductus release --branch`, made without git, or stored before 0.7.4.4.3). Labels, unsigned. |

Indexes: primary key `id`; `releases_stored_at (stored_at)`.

## `release_folders`

A folder's files in a release, once by the list's content (migration 0030): an interface's folder (`interfaces/<name>`, its subfolders included), a folder at the project's top (`lib`, `environments`, `secrets`), or the top's own files (`ductus.yaml`, `resources.yaml`). Kept.

| column | type | |
|---|---|---|
| `hash` | bytea | SHA-256 of the list as stored. |
| `files` | jsonb | `[{"path": "interfaces/adt/interface.yaml", "hash": "<hex>"}, {"path": "...", "hash": "...", "binary": true}]` in path order, paths relative to the project: a text's content is in `code_files`, a file marked `binary` in `release_blobs`. |

Index: primary key `hash`.

## `release_blobs`

Files releases carry that aren't text (a NUL byte, or not UTF-8: an image, a spreadsheet), once by their content, byte for byte (migration 0030). Kept.

| column | type | |
|---|---|---|
| `hash` | bytea | SHA-256 of `content`. |
| `content` | bytea | The file's bytes. |

Index: primary key `hash`.

## `release_checks`

Each check of a stored release against an environment (`POST /releases/{id}/check`, migration 0030): the gate's report, kept with the release.

| column | type | |
|---|---|---|
| `id` | integer | Identity. |
| `release_id` | bytea | `releases.id`. |
| `at` | timestamptz | When it ended. |
| `environment` | text | The environment checked against (the engine's). |
| `instance` | text | The engine run that checked it (as `engines` and the audit log name it). |
| `host` | text | Its server. |
| `by_name` | text | Who asked. |
| `ok` | boolean | It passed: no problem found. |
| `problems` | integer | How many problems. |
| `seconds` | real | How long it took. |
| `report` | jsonb | `ductus release check --json`'s document, without the manifest (the release's own). |
| `ductus_version` | text | The Ductus version that ran it (migration 0031): a deploy reuses only a check its own version ran. Null for checks before 0.7.4.4. |

Indexes: primary key `id`; `release_checks_release (release_id, at)`.

## `release_places`

Where a release was deployed on **other** databases, as the file carrying it said when it was stored here (migration 0032, 0.7.4.4.3): what an environment that requires others first (`DUCTUS_DEPLOY_REQUIRES`) reads, beside this database's own `deployments`. Each upload of the file adds the records it carries that aren't here yet (a release stored before keeps its first manifest, so the record can't live only there). **Unsigned**: anyone holding the file can edit its manifest ([why, and what to rely on instead](nuances/releases.md)). Kept.

| column | type | |
|---|---|---|
| `release_id` | bytea | `releases.id`. |
| `environment` | text | The environment it was deployed to. |
| `host` | text | That engine's server. |
| `deployment` | integer | The deployment's id on that database. |
| `at` | timestamptz | When it was deployed there. |
| `by_name` | text | Who deployed it there. |
| `ductus_version` | text | The Ductus version that ran it there. |
| `received_at` | timestamptz | When the file carrying the record was stored here. |
| `received_by` | text | Who stored it. |

Index: primary key `(release_id, environment, host, deployment)`.

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
