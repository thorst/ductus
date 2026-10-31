# ductus.yaml: the project's settings

`ductus.yaml` sits at the top of the project and holds what applies to every interface in it
and to the engine that runs them: how long messages are kept, where the web UI listens, how
people sign in, the audit log, alerts, and the engine's own checks. **It's optional**: every
setting has a default, and `ductus init` writes the main ones out so you can see them.

```yaml
# ductus.yaml
ports:
  mllp: 6661-6699
  http: 8080-8099
retention: 7w
quarantined: keep
api:
  listen: 127.0.0.1:7700
isolates:
  heap_mb: 512
require_about: false
```

That is `ductus init`'s file, and also exactly the defaults. Every setting is in the
[reference](../reference/config/ductus.md); this page says what each section is for and when
to change it.

Only the `alerts:` section may use `${NAME}` and `${secret:NAME}`
([environments](environments.md)): mail servers and webhooks differ per environment, but how long
messages are kept and who may sign in shouldn't.

## Ports for new interfaces

[`ports`](../reference/config/ductus.md#ports) is where `ductus new interface` picks a new
interface's port from: the first free one in the range. It doesn't limit anything: each
interface's port is written in its own interface.yaml, and the engine listens on whatever that
says. **Set it** to the ranges your firewall team opened, so new interfaces land inside them with
no new request ([ports and firewalls](../ports.md)). Default: MLLP `6661-6699`, HTTP `8080-8099`.

## How long messages are kept

[`retention`](../reference/config/ductus.md#retention) is how long every interface keeps its
messages, unless its interface.yaml says otherwise: **`1w`, `3w` or `7w`** (the default), or the
same in days (`7d`, `21d`, `49d`).

```yaml
# ductus.yaml, in part
retention: 3w
quarantined: keep
```

- **Why only three:** each window is a set of partitions of the message tables, and old messages
  go by dropping a whole partition at once, which costs nothing however busy the feed. Deleting
  rows one at a time, as engines with any number of days must, slows the busiest tables and
  bloats them ([nuance](../nuances/retention.md#retention-is-1-3-or-7-weeks-not-any-number-of-days-level-1)).
- **Kept at least the window**, and gone within a day (`1w`) or a week (`3w`, `7w`) after it.
- **Nothing waiting is ever deleted.** A message still waiting for a destination past the window
  moves to the held area and is sent when it can be
  ([nuance](../nuances/retention.md#pending-messages-are-never-purged-past-the-window-theyre-held-level-2)).
- **Choose** the window that covers how far back your team looks into problems: `1w` for busy
  feeds nobody looks back on, `7w` where problems surface weeks later (billing, a monthly
  reconciliation). Longer history belongs in the source system or an archive, not here.
- **Shortening it later deletes messages**, so the engine waits for
  `ductus retention confirm <interface>` before it applies; a longer window applies at once
  ([nuance](../nuances/retention.md#shortening-retention-deletes-messages-so-it-needs-a-confirmation-level-2)).

[`quarantined`](../reference/config/ductus.md#quarantined) says what happens to quarantined
messages (ones a destination refused for good, or the code rejected) once the window has passed:
**`keep`** (the default) holds them in the held area until a person resends, corrects or
dismisses them; `purge` lets them go with the rest. Keep is the safe choice: a quarantined
message is a question nobody has answered yet. `purge` suits a feed whose sender resends anyway.
Watch what's held with `ductus retention` and the `ductus_held_messages` metric.

## The web UI and the API

[`api`](../reference/config/ductus.md#api) is where the REST API and the web UI listen (one port
for both), whether over HTTPS, and who may connect. **By default only this machine can reach
them**: `127.0.0.1:7700`, plain HTTP.

```yaml
# ductus.yaml, in part
api:
  listen: 0.0.0.0:7700
  tls: { certificate: ductus-api }
  allow: [127.0.0.1, 10.1.50.0/24]
```

- **`listen`**: `host:port`, or `off` to run without the API and the web UI (then the command
  line's `status --token`, `drain` and `pause` can't reach this engine either). Listen on the
  server's address, or `0.0.0.0`, for people to use the web UI from their own computers.
  `DUCTUS_API_LISTEN` (or `--api-listen`) overrides it for one engine.
- **`tls`**: the certificate it presents, a `certificates:` entry in resources.yaml. **Needed as
  soon as `listen` isn't this machine only**: the web UI's sign-in cookie is only sent over HTTPS,
  and tokens would otherwise cross the network readable. The start and `ductus validate` warn
  without it ([TLS for the web UI](tls.md#the-web-ui-and-the-api)).
- **`allow`**: the addresses, subnets and host names that may connect (operators', developers',
  the monitoring server). List `127.0.0.1` too, or commands on the server itself are refused.

Every call needs a token or a signed-in person either way: the network settings decide who can
reach the port, not who may do what ([people](people.md)).

By default the API and the console listen on this machine only. Anyone else reaches them once you
set `api.listen` to an address on the network, and then they need `tls`
([nuance](../nuances/drain-and-api.md#the-api-listens-on-this-machine-only-until-you-open-it-level-4)).

## Signing in

[`auth`](../reference/config/ductus.md#auth) sets how people sign in to the web UI and the API:
how long a session lasts (12 hours idle, 7 days at most), the shortest password (12 characters),
how many wrong passwords lock a username (5 in 15 minutes, for 15 minutes), whether people may
make their own API tokens, and a directory (Active Directory, OpenLDAP) to sign in with. The
defaults suit most sites; [People: users, tokens, the directory](people.md) explains each, and
when to change it.

## The audit log

[`audit`](../reference/config/ductus.md#audit) is how long the record of who did what is kept
(sign-ins, resends, edits, pauses, account changes, payload reads) and whether reading a
message's payload is recorded.

```yaml
# ductus.yaml, in part
audit:
  keep: 6y              # the default: HIPAA's usual documentation period
  payload_reads: true   # the default: who looked at patient data
```

- **`keep`**: at least a year, in years (`6y`) or any duration. Six years is the usual HIPAA
  documentation period; ask your compliance office for yours. Older rows are deleted hourly.
- **`payload_reads`**: with patient data, who looked matters as much as who changed. Turn it off
  only where messages hold no personal data. Reading needs the `payloads` permission either way
  ([nuance](../nuances/people.md#payload-reads-are-logged-before-they-are-shown-level-3)).

The audit log and the messages it names are kept for different times: the log for `audit.keep`
(six years), a message for its interface's retention, so an audit row can outlive the payload it
mentions ([nuance](../nuances/people.md#copies-of-payloads-and-the-audit-log-are-kept-for-different-times-level-3)). An engine that crashed shows in the log as a
start whose previous run never stopped ([nuance](../nuances/people.md#a-crash-shows-in-the-audit-log-level-3)).

## Alerts and owners

[`alerts`](../reference/config/ductus.md#alerts) says where notifications go (a mail server,
channels: email lists and chat webhooks), which health states alert and how badly, and when to
remind and to stay quiet. [`owners`](../reference/config/ductus.md#owners) gives the address of
owners that aren't accounts (a team's mailing list, by the name interfaces give). Nothing alerts
until you write rules: [Alerts and owners](alerts.md) is the whole story.

## The engine's own connection checks

[`doctor`](../reference/config/ductus.md#doctor) is what the engine checks by itself, as
`ductus doctor` would: **every six hours, only as far as each server's certificate** (DNS, TCP,
the TLS handshake; nothing logs in). That's what makes certificates alert 30, 14 and 7 days
before they expire, including the ones of servers the engine connects to.

```yaml
# ductus.yaml, in part
doctor:
  schedule: 6h          # the default; `off` to turn it off
  mode: certificates    # the default; `full` also logs in
```

- **`mode: full`** also logs in to each database, SFTP server and the directory, so a changed
  password is noticed before a message needs it. Use it where the servers' owners agree to a
  login every `schedule` (a service account's logins show in their logs).
- **Turn it off** only if something else watches every certificate the engine connects to.
- The first check runs a minute or two after the engine starts, at a random moment, so engines
  started together don't check together
  ([nuance](../nuances/certificates.md#the-engine-checks-certificates-by-itself-and-logs-in-to-nothing-level-2)).

## Memory for interface code

[`isolates.heap_mb`](../reference/config/ductus.md#isolates) is the memory limit, in MB, of each
copy of the JavaScript engine that runs interface code (an *isolate*). Default: `512`.

There is one isolate per thread of the shared pool (one thread per CPU core, or
`DUCTUS_THREADS`), and one per worker of each interface with `isolation: dedicated`. So interface
code can use up to about `heap_mb` × (threads + dedicated workers) in all; most interfaces use
a few MB, so that's a ceiling, not what you'll see.

- **Code that fills its isolate** is stopped, its message quarantined and its interface stopped
  until a restart. The engine and every other interface keep running.
- **In the shared pool the limit is shared** by every interface on the thread, so the one that
  runs out isn't always the one that filled it: a cache in another interface that only grows
  ([nuance](../nuances/runaway.md#the-heap-limit-is-shared-by-everything-in-an-isolate-level-4),
  [what you keep is what costs](../nuances/transforms.md#short-lived-objects-are-cheap-what-you-keep-is-what-costs-level-4)).
- **Raise it** for code that holds a lot on purpose (a large lookup table loaded once), or give
  that interface `isolation: dedicated` with its own isolates. **Lower it** on a small server
  with many cores, so the ceiling fits in its memory.

## Insisting on documentation

[`require_about`](../reference/config/ductus.md#require_about) makes `ductus validate` fail, not
only warn, when an interface doesn't name the application or vendor on each side in its
`about:`. That's what lets `ductus impact --vendor "Epic Systems"` answer "what does this
vendor's outage touch?" completely. **Turn it on** once the existing interfaces are documented,
so every new one must be. Default: `false`.
