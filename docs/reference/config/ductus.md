# `ductus.yaml`

`ductus.yaml`: the project's settings, for every interface in it and the engine that runs them: how long messages are kept, the REST API and web UI, how people sign in, the audit log, alerts, and the engine's own connection checks.

The file sits at the top of the project and is optional: every setting has a default, which `ductus init` writes out for the main ones. Only `alerts:` may use the environment's `${NAME}` and `${secret:NAME}`; everything else is read as written.

Schema for editors: `docs/schema/ductus.json` (written into a project by `ductus types`). Files: `ductus.yaml`.

### `ports`

**Type:** settings, below

The port ranges `ductus new interface` picks a free port from for a new interface's listener (each interface's own port is in its interface.yaml). Set them to the ranges your firewall team opened, so new interfaces land inside. Default: MLLP `6661-6699`, HTTP `8080-8099`. Example: `ports: { mllp: 7000-7099 }`.

### `ports.mllp`

**Type:** port range (6661-6699)

The range a new MLLP interface's port is picked from, as `first-last`. Default: `6661-6699`. Example: `mllp: 7000-7099`.

### `ports.http`

**Type:** port range (6661-6699)

The range a new HTTP interface's port is picked from, as `first-last` (HTTP interfaces can also share one port). Default: `8080-8099`. Example: `http: 9080-9099`.

### `isolates`

**Type:** settings, below

Limits of the JavaScript engine that runs interface code (`heap_mb`). Default: `heap_mb: 512`.

### `isolates.heap_mb`

**Type:** number

The memory limit, in MB, of each isolate: one copy of the JavaScript engine (V8) that runs interface code. At least `64`.

Every thread of the shared pool has an isolate (one thread per CPU core, or `--threads`), and so does every worker of an `isolation: dedicated` interface, so the code can use up to about `heap_mb` × (threads + dedicated workers) in all: size the server with that. Code that fills its isolate is stopped, its message quarantined and its interface stopped until a restart; the engine and every other interface keep running. In the shared pool the limit is shared by every interface on the thread, so the one that runs out isn't always the one that filled it (a cache in another interface that only grows).

Raise it for code that holds a lot on purpose (a large lookup table loaded once), or give that interface `isolation: dedicated`; lower it on a small server with many cores. Default: `512`. Example: `isolates: { heap_mb: 1024 }`.

### `require_about`

**Type:** `true` or `false`

Make `ductus validate` fail (not only warn) when an interface doesn't name the application or vendor on each side in `about:`, so `ductus impact` never misses one. Turn it on once the existing interfaces are documented, and every new one must be. Default: `false`. Example: `require_about: true`.

### `api`

**Type:** settings, below

The REST API and the web UI: where they listen, HTTPS, and who may connect. Default: `127.0.0.1:7700`, plain HTTP, this machine only.

### `api.listen`

**Type:** text

Where the REST API and the web UI listen, as `host:port`, or `off` to run without them (then `ductus status --token`, `drain`, `pause` and the web UI can't reach this engine).

The default listens on this machine only, so opening it to the network is a choice you make. For people to use the web UI from their own computers, listen on the server's address (or `0.0.0.0` for all of them) and add `tls` (the UI's sign-in needs HTTPS anywhere but on the server itself) and `allow`. Every call needs a token or a signed-in person either way. `ductus run --api-listen` (or `DUCTUS_API_LISTEN`) overrides it for one engine. Default: `127.0.0.1:7700`. Example: `listen: 0.0.0.0:7700`.

### `api.tls`

**Type:** settings, below

HTTPS only, presenting the `certificate` named here (a `certificates:` entry in resources.yaml); with `clients`, callers must present a certificate too. Needed for the web UI anywhere but on the server itself (its sign-in cookie is only sent over HTTPS), and for tokens not to cross the network readable; the start warns without it when `listen` isn't this machine only. Default: plain HTTP. Example: `tls: { certificate: ductus-api }`.

### `api.tls.certificate`

**Type:** text · **Required**

The certificate this listener presents: a `certificates:` entry in resources.yaml with the certificate and its key, for the name senders connect to. A certificate the engine can't use (expired, its key doesn't match) stops the start with the reason; renewed files are picked up within a minute, without a restart. Required. Example: `certificate: ductus-mllp`.

### `api.tls.clients`

**Type:** settings, below

Ask senders for a certificate of their own, and refuse any connection without one that `ca` issued and, with `subjects`, that names an allowed system. A client certificate says which system is connecting, beside `allow:`'s where from. Default: no client certificates asked for. Example: `clients: { ca: hospital-ca, subjects: ["CN=*.lab.hospital.org"] }`.

### `api.tls.clients.ca`

**Type:** text · **Required**

The CA that issues senders' certificates: a `certificates:` entry holding the CA's certificate (`ca` or `ca_file`). Only certificates it issued are taken. Required. Example: `ca: hospital-ca`.

### `api.tls.clients.subjects`

**Type:** list of text

Which of that CA's certificates are taken: a certificate matching any one pattern. A pattern is `ATTR=value` parts separated by commas, all of which must match, with `*` and `?` wildcards and case ignored: `CN`, `O`, `OU`, `C`, `L`, `ST`, `DC`, or `DNS` for the certificate's DNS names. Needed when the CA issues certificates to more than your senders (a hospital-wide CA). Default: any certificate the CA issued. Example: `subjects: ["CN=bridges*.hospital.org", "O=Quest, OU=Lab"]`.

### `api.allow`

**Type:** list of text

Who may connect: addresses, subnets and host names (the operators' and developers' networks, the monitoring server). Anyone else is disconnected before anything is read. List `127.0.0.1` too, or commands run on the server itself (`ductus status --token`, `drain`) are refused. Default: anyone who can reach `listen`. Example: `allow: [127.0.0.1, 10.1.50.0/24]`.

### `retention`

**Type:** 1w, 3w or 7w

How long messages are kept, for every interface whose interface.yaml has no `retention`: `1w`, `3w` or `7w` (or the same in days: `7d`, `21d`, `49d`).

Only these three, on purpose: each is a set of partitions of the message tables, and old messages go by dropping a whole partition at once, which costs nothing however busy the feed (deleting rows one by one would slow the busiest tables). A message is kept at least its window and is gone within a day (`1w`) or a week (`3w`, `7w`) after it. Messages still waiting for a destination, and quarantined ones (see `quarantined`), are never deleted: past the window they move to the held area until they're delivered or resolved.

Pick the window that covers how far back you look into problems; longer history belongs in the source system or an archive. Shortening it later deletes messages, so it waits for `ductus retention confirm <interface>`. Default: `7w`. Example: `retention: 3w`.

### `quarantined`

**Type:** `keep`, `purge`

What happens to quarantined messages once the retention window has passed, for every interface whose interface.yaml doesn't say: `keep` them in the held area until someone resends, corrects or dismisses them, or `purge` them with the rest.

In most engines unresolved quarantines pile up for years; here they're kept until a person decides, with no second time limit. Watch them with `ductus retention` and the `ductus_held_messages` metric. `purge` suits feeds whose senders resend anyway. Default: `keep`. Example: `quarantined: purge`.

### `auth`

**Type:** settings, below

How people sign in to the web UI and the API: sessions, passwords, lockout, API tokens, and a directory (Active Directory, OpenLDAP). Local accounts are made with `ductus user` on the server. Default: local accounts and tokens, with the session and password rules below.

### `auth.session`

**Type:** settings, below

How long a browser session lasts: `idle` (unused) and `max` (in all). Default: 12 hours idle, 7 days at most.

### `auth.session.idle`

**Type:** duration

A session ends after this long unused, and the person signs in again: a shift, with room to spare, so an analyst isn't signed out mid-task. Shorten it where screens are shared or left unlocked. No longer than `max`. Default: `12h`. Example: `idle: 30m`.

### `auth.session.max`

**Type:** duration

A session ends this long after sign-in however much it's used, so a stolen session cookie doesn't last forever. Default: `7d`. Example: `max: 1d`.

### `auth.password`

**Type:** settings, below

What a local account's password must be (`min_length`). Directory accounts follow the directory's rules. Default: at least 12 characters.

### `auth.password.min_length`

**Type:** number

The shortest password a local account may have, at least 8. Length matters more than mixing kinds of characters, so there's no rule for those (a few ordinary words together work well); a password can't contain the username, be one character repeated, or be one of the most common ones. Applies to new passwords; existing ones keep working. Default: `12`. Example: `min_length: 16`.

### `auth.lockout`

**Type:** settings, below

How many wrong passwords lock a username, and for how long. Default: 5 within 15 minutes lock it for 15 minutes.

### `auth.lockout.attempts`

**Type:** number

How many wrong passwords for one username, within `window`, lock it, at least 1. It slows anyone guessing; a person who forgot waits `for`, or an admin unlocks them (`ductus user unlock`). Directory accounts count here and in the directory. Default: `5`. Example: `attempts: 10`.

### `auth.lockout.window`

**Type:** duration

How far back wrong passwords are counted. Default: `15m`. Example: `window: 1h`.

### `auth.lockout.for`

**Type:** duration

How long a locked username stays locked. Default: `15m`. Example: `for: 30m`.

### `auth.token_sign_in`

**Type:** `true` or `false`

Whether the sign-in page also takes an API token instead of a username and password: how the first people get in before accounts exist. Turn it off once everyone has an account; `ductus user` on the server always works. Default: `true`. Example: `token_sign_in: false`.

### `auth.user_tokens`

**Type:** `true` or `false`

Whether people may make API tokens of their own (for scripts, under Account in the web UI). A person's token can do no more than they can, and stops when their account is disabled. Turn it off to keep tokens to the ones admins make. Default: `true`. Example: `user_tokens: false`.

### `auth.ldap`

**Type:** settings, below

Sign-in with a directory (Active Directory, OpenLDAP), beside local accounts: which `ldap:` entry in resources.yaml, where people are, and which groups give which role. People in none of the groups can't sign in. Default: local accounts only. Example: `ldap: { resource: corp-ad, user_base: "OU=Staff,DC=hospital,DC=org", groups: { admin: "CN=Ductus Admins,OU=Groups,DC=hospital,DC=org" } }`.

### `auth.ldap.resource`

**Type:** text · **Required**

The directory to use: the name of an `ldap:` entry in resources.yaml (its address, TLS and service account). Required. Example: `resource: corp-ad`.

### `auth.ldap.user_base`

**Type:** text · **Required**

Where people are searched for, as a DN: the whole subtree under it. Narrow it to the branch your staff are in, so service accounts and contacts can't sign in by accident. Required. Example: `user_base: "OU=Staff,DC=hospital,DC=org"`.

### `auth.ldap.user_filter`

**Type:** text

How a person is found from the name they type: an LDAP filter where `{username}` stands for it, escaped so it can't change the filter. More than one entry found is refused. `(sAMAccountName={username})` is Active Directory's sign-in name; OpenLDAP and FreeIPA use `(uid={username})`; `(|(sAMAccountName={username})(mail={username}))` takes either. Default: `(sAMAccountName={username})`. Example: `user_filter: "(uid={username})"`.

### `auth.ldap.attributes`

**Type:** settings, below

Which of the person's attributes hold their name, email, sign-in name and lasting id. The defaults suit Active Directory. Default: `displayName`, `mail`, the filter's attribute, and `auto` for the id.

### `auth.ldap.attributes.display_name`

**Type:** text

The attribute with the person's name as others see it in the web UI and the audit log. Default: `displayName`. Example: `display_name: cn`.

### `auth.ldap.attributes.email`

**Type:** text

The attribute with the person's email address: shown in the web UI, matched against owner texts (`about.owner: anna@hospital.org`), and used to find owners in the directory before their first sign-in. Default: `mail`. Example: `email: userPrincipalName`.

### `auth.ldap.attributes.username`

**Type:** text

The attribute with the sign-in name, so a refresh notices it change and renames the account. Needed only when `user_filter` doesn't match `{username}` against one attribute plainly (`ductus validate` warns then). Default: the attribute the filter first matches `{username}` against (`sAMAccountName` for the default filter). Example: `username: uid`.

### `auth.ldap.attributes.id`

**Type:** text

The attribute that tells one person from another through any rename: `auto` (Active Directory's `objectGUID`, else the standard `entryUUID`), another attribute's name (`nsUniqueId`), or `none`. With an id, a renamed person keeps their account, and a sign-in name given to someone new gets a new account instead of the old one's role and history. `none` matches by sign-in name only, so those two can't be told apart (validate warns). Default: `auto`. Example: `id: nsUniqueId`.

### `auth.ldap.groups`

**Type:** settings, below · **Required**

Which directory groups give which role (`admin`, `operator`, `viewer`) and which add a permission (`permissions.payloads`, `permissions.audit`). The highest role a person's groups give wins; a person in none can't sign in. At least one role group. Required. Example: `groups: { operator: "CN=Interface Team,OU=Groups,DC=hospital,DC=org" }`.

### `auth.ldap.groups.admin`

**Type:** text, or a list

The group (a DN), or groups, whose members are admins: everything operators can do, plus accounts, tokens and every permission. Write several one per line in a YAML list, not as `[a, b]` (DNs have commas). Default: none. Example: `admin: "CN=Ductus Admins,OU=Groups,DC=hospital,DC=org"`.

### `auth.ldap.groups.operator`

**Type:** text, or a list

The group or groups whose members are operators: what viewers can do, plus resend, edit, dismiss, pause, drain and run connection checks. Default: none. Example: `operator: "CN=Interface Team,OU=Groups,DC=hospital,DC=org"`.

### `auth.ldap.groups.viewer`

**Type:** text, or a list

The group or groups whose members are viewers: they see interfaces, health, queues and messages' details, but change nothing (and see payloads only with `payloads`). Default: none. Example: `viewer: "CN=Help Desk,OU=Groups,DC=hospital,DC=org"`.

### `auth.ldap.groups.permissions`

**Type:** settings, below

Groups that add a permission on top of the role: `payloads` (read message contents, usually patient data) and `audit` (read the audit log). Admins have both anyway. Default: none.

### `auth.ldap.groups.permissions.payloads`

**Type:** text, or a list

The group or groups whose members may read message payloads (and edit them, as operators): the people who need to see patient data to do their work. Every read is in the audit log. Default: none. Example: `payloads: "CN=Ductus PHI,OU=Groups,DC=hospital,DC=org"`.

### `auth.ldap.groups.permissions.audit`

**Type:** text, or a list

The group or groups whose members may read and export the audit log: compliance and security staff. Default: none. Example: `audit: "CN=Ductus Auditors,OU=Groups,DC=hospital,DC=org"`.

### `auth.ldap.nested`

**Type:** `true` or `false`

Count members of groups inside the configured groups too (a department group placed in "Ductus Operators"). Active Directory answers this itself; other directories are walked group by group, at most 8 levels deep. Default: `false` (direct members only). Example: `nested: true`.

### `auth.ldap.group_base`

**Type:** text

Where groups are searched for when nested groups are walked (not Active Directory, which answers by itself), as a DN. Default: the directory's own top (its naming context). Example: `group_base: "ou=groups,dc=example,dc=org"`.

### `auth.ldap.refresh`

**Type:** duration

How often the engine asks the directory again about everyone with a session or a token, and about directory people who own something (at least `1m`). Someone removed from the groups, disabled or deleted there loses access within this long, sessions and tokens alike; a renamed person keeps their account. Shorter means quicker revocation and more directory searches. Default: `15m`. Example: `refresh: 5m`.

### `audit`

**Type:** settings, below

The audit log of who did what (sign-ins, resends, edits, pauses, account changes, payload reads): how long it's kept and whether reading a message's payload is logged. Default: kept 6 years, payload reads logged.

### `audit.keep`

**Type:** duration (or years: 6y)

How long audit rows, and the record of what was done to messages, are kept: at least a year, in years (`6y`, 365 days each) or any duration (`2190d`). Older rows are deleted hourly. Six years is the usual HIPAA documentation period; ask your compliance office for yours. The log is the record of who did what, so it can't be shorter than a year. Default: `6y`. Example: `keep: 7y`.

### `audit.payload_reads`

**Type:** `true` or `false`

Log who reads a message's payload (in the web UI or through the API), before it's shown: with patient data, who looked matters as much as who changed. Turn it off only where messages hold no personal data. Reading needs the `payloads` permission either way. Default: `true`. Example: `payload_reads: false`.

### `audit.payload_read_window`

**Type:** duration

One person reading the same message again within this long counts as one read, one row (a page that reloads itself would otherwise write a row each time). `0s` writes a row for every read. Default: `10m`. Example: `payload_read_window: 0s`.

### `alerts`

**Type:** settings, below

Where notifications go and when: a mail server, channels (email lists, chat webhooks), and rules saying which health states alert, how badly and to whom. The engine judges every interface and destination every 15 seconds and tells people once, then reminds, and says when it's fine again.

The only section that may use `${NAME}` and `${secret:NAME}`, read for the environment the engine runs in (webhook URLs and mail passwords are secrets). Default: no alerts (states show in the web UI only). Example: `alerts: { channels: { oncall: { email: oncall@hospital.org } }, rules: [{ when: [error, stalled], severity: critical, to: [oncall] }] }`.

### `alerts.smtp`

**Type:** settings, below

The mail server that email channels (and owners' email) go through. Needed only for email; webhooks need none. Default: none. Example: `smtp: { host: smtp.hospital.org, from: ductus@hospital.org }`.

### `alerts.smtp.host`

**Type:** text · **Required**

The mail server's host name: your organization's relay or submission server. Its certificate must be for this name. Required. Example: `host: smtp.hospital.org`.

### `alerts.smtp.port`

**Type:** number

The mail server's port. Default: `587` with `tls: starttls`, `465` with `tls: tls`, `25` with `tls: none`. Example: `port: 2525`.

### `alerts.smtp.tls`

**Type:** `starttls`, `tls`, `none`

How the connection is protected:

- `starttls`: a plain connection turned into TLS before anything else (submission, port 587); refused if the server can't.
- `tls`: TLS from the first byte (port 465).
- `none`: no encryption, for a relay on this machine or its network segment only; mail and any password cross readable.

Default: `starttls`. Example: `tls: tls`.

### `alerts.smtp.username`

**Type:** text

The account to log in as, when the server asks for one (most submission servers do; an internal relay often doesn't). Goes with `password`. Default: none (no login). Example: `username: ductus@hospital.org`.

### `alerts.smtp.password`

**Type:** text

The account's password, from a secret (`${secret:SMTP_PASSWORD}`). Goes with `username`. Example: `password: "${secret:SMTP_PASSWORD}"`.

### `alerts.smtp.from`

**Type:** text · **Required**

The address notifications come from. Use one your mail server lets this account send as, and that recipients' filters let through (ask the mail team to add it to their allow list). Required. Example: `from: ductus@hospital.org`.

### `alerts.smtp.ca`

**Type:** text

A CA to trust for the mail server besides the ones this server trusts (a `certificates:` entry in resources.yaml), for an internal relay with a private CA's certificate. Not with `tls: none`. Default: the system's trusted CAs only. Example: `ca: hospital-ca`.

### `alerts.smtp.certificate`

**Type:** text

A client certificate to present to the mail server (a `certificates:` entry with its key), for servers that admit senders by certificate. Not with `tls: none`. Default: none. Example: `certificate: ductus-client`.

### `alerts.channels`

**Type:** names, each with settings, below

Where alerts can go, by a name rules use in `to:`: each an email list, a chat webhook, or both. Send to teams (a mailing list, a chat channel) rather than to one person: people change and sleep; a team's list doesn't. `ductus alerts test <channel>` sends a test from the command line, no engine needed. Default: none. Example: `channels: { oncall: { email: interfaces-oncall@hospital.org }, chat: { webhook: { url: "${secret:CHAT_WEBHOOK}", format: teams } } }`.

### `alerts.channels.<name>.email`

**Type:** text, or a list

The address, or addresses, notifications are mailed to: best a team's list (`interfaces-oncall@hospital.org`), or a pager's email gateway. Needs `alerts.smtp`. Default: none. Example: `email: [interfaces-oncall@hospital.org, lab-it@hospital.org]`.

### `alerts.channels.<name>.webhook`

**Type:** settings, below

A chat or incident tool's incoming webhook that notifications are posted to (Teams, Slack, or your own tool with `format: json`). Default: none. Example: `webhook: { url: "${secret:CHAT_WEBHOOK}", format: teams }`.

### `alerts.channels.<name>.webhook.url`

**Type:** text · **Required**

The webhook's URL. A chat webhook's URL is its own password (anyone with it can post to the channel), so keep it in a secret: `${secret:CHAT_WEBHOOK}`. The engine shows only its scheme and host anywhere. Required. Example: `url: "${secret:CHAT_WEBHOOK}"`.

### `alerts.channels.<name>.webhook.format`

**Type:** `json`, `slack`, `teams`

What the posted body looks like:

- `json`: Ductus's own JSON, every field, for scripts and incident tools.
- `slack`: Slack incoming webhooks (also Mattermost and Rocket.Chat).
- `teams`: Microsoft Teams Workflows webhooks (an Adaptive Card; the old Office 365 connectors are being retired).

Default: `json`. Example: `format: teams`.

### `alerts.channels.<name>.webhook.ca`

**Type:** text

A CA to trust for the webhook's server besides the ones this server trusts (a `certificates:` entry in resources.yaml), for an internal tool with a private CA's certificate. Default: the system's trusted CAs only. Example: `ca: hospital-ca`.

### `alerts.channels.<name>.webhook.certificate`

**Type:** text

A client certificate to present to the webhook's server (a `certificates:` entry with its key), for tools that admit callers by certificate. Default: none. Example: `certificate: ductus-client`.

### `alerts.rules`

**Type:** list of settings, below

Which health states alert, how badly, and to whom. Each state is matched by the first rule that names it; a state no rule names doesn't alert (`ductus validate` says which). Default: none (nothing alerts). Example: `rules: [{ when: [error, stalled, stopped], severity: critical, to: [oncall, chat] }, { when: [behind, silent, missed, certificate], severity: warning, to: [chat] }]`.

### `alerts.rules.when`

**Type:** list of `error`, `stalled`, `behind`, `stopped`, `silent`, `missed`, `certificate` · **Required**

The health states this rule covers:

- `error`: a destination has quarantined messages someone must look at.
- `stalled`: a destination has had messages waiting for over a minute and sent none (its receiver is down, or a message keeps failing).
- `behind`: a destination's queue is past its `monitor.behind`.
- `stopped`: an interface's code was stopped (it ran away or ran out of memory).
- `silent`: nothing received within `monitor.active_within`.
- `missed`: nothing received by `monitor.active_by`.
- `certificate`: a certificate the engine uses or connects to nears expiry, or expired.

A paused destination never alerts: it's paused on purpose. Required. Example: `when: [error, stalled, stopped]`.

### `alerts.rules.severity`

**Type:** `warning`, `critical` · **Required**

How bad these states are: `warning` (worth knowing today; held during quiet hours) or `critical` (act now; never held). An alert that goes from warning to critical is told again. An expired certificate is always critical. Required. Example: `severity: critical`.

### `alerts.rules.to`

**Type:** list of text · **Required**

Who is told: channel names from `channels`, and `owner` (whoever owns the destination or its interface: its `owner`, else `about.owner`, mailed at their account's email, an `owners:` entry, or the text itself if it's an address). Lead with team channels; add `owner` where a person should hear directly. Required. Example: `to: [oncall, chat, owner]`.

### `alerts.repeat`

**Type:** duration, or 0

While an alert stays open and nobody has acknowledged it, remind its recipients this often; `0` never reminds. Acknowledging (on the Alerts page) stops reminders until it gets worse or recovers. Keep it long where a channel pages someone. Default: `4h`. Example: `repeat: 1h`.

### `alerts.recovery`

**Type:** `true` or `false`

Tell the people who heard about an alert when it's fine again (after `settle`), so they can stand down. Default: `true`. Example: `recovery: false`.

### `alerts.settle`

**Type:** duration, or 0

How long a state must be fine before its alert counts as over. Bad again sooner, and the same alert goes on: a receiver flapping up and down is one alert, not dozens. `0` ends it at once. Default: `2m`. Example: `settle: 10m`.

### `alerts.quiet`

**Type:** settings, below

Hours when notifications of some severities wait until morning (warnings, overnight). A warning that comes and goes in the night is never sent, only kept in the history; one that turns critical goes at once. Default: none. Example: `quiet: { between: "22:00-06:00", severities: [warning] }`.

### `alerts.quiet.between`

**Type:** hours (06:00-22:00) · **Required**

The quiet hours, in the server's local time (mind a server or container that runs on UTC): `"22:00-06:00"`. Notifications due in them wait until they end. Required. Example: `between: "21:00-07:00"`.

### `alerts.quiet.severities`

**Type:** list of `warning`, `critical`

Which severities wait during the quiet hours. Keep `critical` out: a critical alert that waits until morning wasn't critical. Default: `[warning]`. Example: `severities: [warning]`.

### `alerts.heartbeat`

**Type:** settings, below

A URL the engine calls every `every` while it runs, for a service elsewhere that raises its own alarm when the calls stop (healthchecks.io, Better Stack, Uptime Kuma, a monitoring tool's push check). An engine that is down, or can't reach its database, can't send alerts itself: this is how anyone hears about it. Set it in production. Default: none. Example: `heartbeat: { url: "${secret:HEARTBEAT_URL}", every: 5m }`.

### `alerts.heartbeat.url`

**Type:** text · **Required**

The URL the engine calls (an HTTP GET) while it runs: the "ping" or "push" URL the watching service gives you. It often holds a secret: `${secret:HEARTBEAT_URL}`. Use a service that doesn't run on the engine's server, or it goes down with it. Required. Example: `url: "${secret:HEARTBEAT_URL}"`.

### `alerts.heartbeat.every`

**Type:** duration

How often it's called. Set the watching service's grace period to about twice this. Default: `5m`. Example: `every: 1m`.

### `alerts.heartbeat.ca`

**Type:** text

A CA to trust for the heartbeat's server besides the ones this server trusts (a `certificates:` entry in resources.yaml). Default: the system's trusted CAs only. Example: `ca: hospital-ca`.

### `alerts.heartbeat.certificate`

**Type:** text

A client certificate to present to the heartbeat's server (a `certificates:` entry with its key). Default: none. Example: `certificate: ductus-client`.

### `alerts.keep`

**Type:** duration (or years: 6y)

How long alerts' history (what fired, who was told, who acknowledged) is kept, in years (`1y`) or any duration. Default: `1y`. Example: `keep: 2y`.

### `alerts.certificates`

**Type:** settings, below

When an expiring certificate alerts (`certificate` in a rule): the steps before expiry. Default: 30, 14 and 7 days.

### `alerts.certificates.warn_days`

**Type:** list of number

The days before a certificate expires at which its alert is told: it fires at the first (with the rule's severity) and is told again at each later one, then as critical once expired. No reminders between steps: the steps are the reminders. Counted in whole days, from the earliest expiry in the certificate's chain. Pick the first step to leave time for your slowest renewal (a vendor's CA, a change board). Default: `[30, 14, 7]`. Example: `warn_days: [45, 14, 3]`.

### `owners`

**Type:** names, each with settings, below

Owners that aren't accounts here, by the name interfaces give in `about.owner`, with the address that reaches them: a team's mailing list (`"Lab Systems": { email: lab-systems@hospital.org }`). Alert rules with `to: owner` mail it. Owners who are accounts (best written as their email) need no entry. Default: none.

### `owners.<name>.email`

**Type:** text, or a list

The address, or addresses, that reach this owner: a team's mailing list. Alert rules with `to: owner` mail it for the interfaces and destinations this owner owns. Default: none. Example: `email: lab-systems@hospital.org`.

### `doctor`

**Type:** settings, below

Connection checks the engine runs by itself, as `ductus doctor` would: by default every six hours, only as far as each server's certificate (nothing logs in), so expiring certificates alert 30, 14 and 7 days ahead. Default: `schedule: 6h`, `mode: certificates`.

### `doctor.schedule`

**Type:** duration, or off

How often the engine checks its connections: a duration of at least `1m`, or `off`. The first check is a minute or two after the engine starts (at random, so engines started together don't check together). Each check takes well under a second for a handful of servers. Turn it off only if something else watches every certificate the engine connects to. Default: `6h`. Example: `schedule: 1h`.

### `doctor.mode`

**Type:** `certificates`, `full`

What a check covers:

- `certificates`: DNS, TCP and the TLS handshake of every connection with a certificate. Nothing logs in, so no service account's log fills up and none is ever locked out.
- `full`: everything `ductus doctor` checks, logins included (databases, SFTP, the directory), so a changed password is noticed before a message needs it.

Use `full` where the servers' owners agree to a login every `schedule`. Default: `certificates`. Example: `mode: full`.

