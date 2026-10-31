# The console (web UI)

The engine serves its web UI on the same address as its REST API:
[`api.listen`](../reference/config/ductus.md#api), `http://127.0.0.1:7700/` by default. Nothing is
installed on anyone's desktop, and there's no second port: whoever can reach the API can open the
console in a browser.

## Getting to it

- **On the server itself** it works as installed: `http://127.0.0.1:7700/`.
- **From people's desks** it needs the API on the network, which means TLS: `api.listen:
  0.0.0.0:7700` with `api.tls` (the browser's sign-in cookie is only sent over https, or to
  localhost) and an `allow:` list of the networks your team works from
  ([nuance](../nuances.md#the-ui-works-over-https-or-at-localhost-on-the-engines-own-server-level-4),
  [the API on the network](../configure/ductus-yaml.md#the-web-ui-and-the-api)).
- **Signing in** is with a username and password (local accounts, or the directory with
  `auth.ldap`), or with an API token ([people](../configure/people.md)). What you see and may do
  follows your role: **viewer** reads; **operator** also pauses, resends, edits, dismisses,
  drains and runs connection checks; **admin** also manages users and tokens. Reading message
  **payloads** is a permission of its own (`payloads`), and so is the **audit** log (`audit`):
  a role alone doesn't give them ([nuance](../nuances.md#reading-payloads-is-a-permission-not-part-of-a-role-level-4)).
- **Live**: the header shows "Live" while the console gets the engine's updates as they happen;
  "Offline" means it lost the engine (stopped, or the network), and it reconnects by itself.

## The pages

The menu has three groups: the day to day first, then reports, then the engine itself.

### Overview

The engine at a glance: what needs attention (the worst first), open alerts, certificates
expiring, and other engines sharing the database. Start a shift here. "Cut off from the database"
across the top means the engine lost its database connection: it's still receiving, and sending
waits until it's back ([nuance](../nuances.md#losing-the-database-connection-pauses-sending-not-receiving-level-3)).

### Interfaces

Every interface with its health, source, how many messages wait and are quarantined, when it
last received, and its owner; worst first. **Mine** shows the interfaces you own (an operator who
owns something starts there). Filter by name, owner, host or port, tick **Only those needing
attention**, or pick a documentation field's value.

### An interface

**Overview**: who sends and who receives (`about:`), each destination's card (health, waiting,
quarantined, last sent, its pause and reason, and **Pause** / **Resume**), the interface's open
alerts, counts and timings since the engine started, and its documentation fields.

**Messages**: the interface's messages, newest first, filtered by status. Each row opens the
message. Finding a message by its content (an MRN, a name) isn't there yet:
[the message store and search](messages.md#finding-a-message).

### A message

What was received and what each destination did with it:

- **Payload**: the message as received, with each HL7 field's path shown as you point at it
  (`PID-3.1`). Needs the `payloads` permission, and opening it is logged *before* it's shown
  ([nuance](../nuances.md#payload-reads-are-logged-before-they-are-shown-level-3)).
- **Destinations**: per destination, its status, attempts, error and when it was sent, and what
  it was sent (recomputed by running its code again, unless it has `save_output: true`).
  **Resend** (a quarantined delivery), **Replay** (a sent or filtered one), **Dismiss**.
- **Edit and resend**: a corrected copy, sent to the destinations you choose.
- **Who viewed the payload** and **History**: every read, resend, edit and dismissal, with who,
  when, and the reason.

What each action does exactly: [resend, replay and correct](resend.md).

### Quarantine

Every quarantined delivery across the interfaces you can see, filtered by interface and
destination, with the reason. Select many and **resend** or **dismiss** them in one go, after
fixing what caused them.

### Alerts

What's open (acknowledge it to stop the reminders; silence it for a while), what's silenced,
the history, and each channel: where it sends, how many were sent or failed, the last error, and
**Test** ([alerts in practice](alerts.md)).

### Hotspots

Where each destination's backlog sits: the lanes messages wait in, each lane's first message
(its key, age, attempts and last error), and what it means ("blocked by message 1234: look at
this message", "every worker blocked: the receiver is down")
([hotspots](monitoring.md#where-a-backlog-sits-hotspots)).

### Reports: Impact, Owners, Documentation

- **Impact**: which interfaces a vendor, application or host touches, and who owns them: the
  first question when a vendor reports a breach or a VPN changes.
- **Owners**: who owns what, and whether each owner reaches a person; **Update** previews moving
  ownership from one person to another (a patch for the project, not a change in place).
- **Documentation**: the company's own documentation fields for every interface, destination
  and resource, and which are missing.

### Engine: Retention, Drain, Connections, Users, Tokens, Audit

- **Retention**: each interface's retention, changes in progress, shorter values waiting for a
  confirmation (type the interface's name to confirm), and what the held area keeps
  ([retention and storage](retention.md)).
- **Drain**: stop taking new messages and deliver what's stored, before a server move or a change
  that needs empty queues; **Stop when drained** ([drain](cli.md#drain-before-a-change)).
- **Connections**: `ductus doctor` from the engine's own server (**Check now**), the last
  scheduled checks, and every certificate the engine presents or connects to, with its expiry.
- **Users** and **Tokens** (admins): accounts, roles, permissions, lockouts; API tokens.
- **Audit** (the `audit` permission): who did what, filtered by person, action, interface or
  message.
- **API reference**: every REST operation, with who may call it.

### Your account

Your role and permissions, your password (local accounts), **My activity** (your own audit
rows), and the appearance: light, dark, high contrast, or your own colors.

## What the console can't do

Change the project: interfaces, code and settings live in Git and are deployed
([the project](../configure/index.md)). The console operates what's deployed; it doesn't edit it.
That keeps every change reviewed and versioned, and is why ownership moves are previews.
