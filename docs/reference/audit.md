# Audit actions

The audit log records who did what, to what, from where, and how it ended: on the Audit page of the web UI (with the `audit` permission), through `GET /api/v1/audit`, and with `ductus audit export` on the server. Each row has one of these actions. Rows are kept `audit.keep` (six years unless ductus.yaml says otherwise).

| Action | What happened |
|---|---|
| `session.sign_in` | Someone signed in to the web UI, with a password or a token. |
| `session.failed` | A sign-in was refused: a wrong password, a locked or disabled account, or a name nobody has (recorded as `anonymous`: what was typed isn't kept, it may be a password). |
| `session.sign_out` | Someone signed out (or signed out their other sessions). |
| `password.change` | Someone changed their own password. |
| `password.reset` | An admin set another person's password (they must change it at their next sign-in). |
| `user.create` | An account was made: by an admin, at the command line, at a directory person's first sign-in, or by the engine for a directory person who owns something (`"reason": "owner"`). |
| `user.update` | An admin changed an account's role, permissions, name or email. |
| `user.disable` | An admin disabled or enabled an account (a disabled account's sessions and tokens stop at once). |
| `user.unlock` | An admin unlocked an account locked by wrong passwords. |
| `user.directory_change` | The directory changed an account: its role, permissions, name or email, a rename, a sign-in name moved aside for someone new, or let it back in. |
| `user.directory_refused` | The directory said no to an account (removed from the groups, disabled or deleted): its sessions and tokens stopped. |
| `token.create` | An API token was made (by an admin, at the command line, or by a person for themselves). |
| `token.revoke` | An API token was revoked. |
| `message.payload.read` | Someone read a message's payload (what it contains), written before it was shown; one row per person and message per `audit.payload_read_window`. |
| `message.release` | Someone resent a message's quarantined deliveries. |
| `message.replay` | Someone sent a message again to a destination that had already had it (sent or filtered). |
| `message.edit` | Someone corrected a message and sent the corrected copy in its place. |
| `message.dismiss` | Someone closed a quarantined delivery without sending it, with a reason. |
| `destination.pause` | Someone paused a destination (with a reason, or added one later). |
| `destination.resume` | Someone resumed a paused destination. |
| `engine.drain` | Someone started a drain: the engine stopped taking messages to deliver what it has. |
| `engine.resume` | Someone ended a drain: the engine takes messages again. |
| `retention.change` | The engine started applying a new retention to an interface's stored messages. |
| `retention.confirm` | Someone confirmed a shorter retention, which deletes messages past the new window. |
| `doctor.run` | Someone ran the connection checks from the web UI or the API. |
| `audit.export` | Someone exported the audit log. |
| `alert.ack` | Someone acknowledged an alert: they're on it, and reminders stop. |
| `alert.silence` | Someone silenced an alert (or a destination before planned work), with a reason and an end. |
| `alert.unsilence` | Someone ended an alert's silence early. |
| `alert.test` | Someone sent a test notification to a channel. |
| `engine.start` | The engine started, with its version and how the previous run on this host ended (a start without a stop before it is a crash). |
| `engine.stop` | The engine stopped cleanly (or handed over to a new engine). |
| `engine.handover` | A new engine took over from the running one (`systemctl reload`: an upgrade or a deploy), with both versions and how long sending paused. |
| `engine.refused` | A start was refused: another live engine runs some of its interfaces, its lanes would strand waiting messages, or a handover would lower lanes. |
| `interface.stopped` | An interface's code was stopped (it ran away or ran out of memory): its messages wait. |
| `interface.partitions_raised` | An interface started with more lanes while messages waited: a key's new messages may pass its older ones until those are sent. |
