# Reports

Impact, owners and connection checks.

## `GET /api/v1/certificates`

Every certificate the engine connects to, as its last check saw it (read even when this server doesn't trust them), every one it presents (`side: ours`, as loaded), and when the engine checks next by itself.

**Who may call it:** anyone signed in (a person or a token).

**Answers**

- `200` (application/json): [`CertificatesOverview`](#certificatesoverview)

## `GET /api/v1/docs`

The company-defined documentation fields and every interface's, destination's and resource's values. Read-only: the project's files are the source of truth.

**Who may call it:** anyone signed in (a person or a token).

**Answers**

- `200` (application/json): [`DocsReport`](#docsreport)

## `POST /api/v1/doctor`

Check every connection the engine needs, from its server: DNS, TCP (with the address a firewall request needs), TLS (certificate, expiry), then each system's own login; also the alerts' mail server, webhooks and heartbeat. Never sends anything to a destination. Listening ports aren't checked (the engine holds them). The run is kept (`/doctor/runs`), and the certificates it saw (`/certificates`). Operator.

**Who may call it:** an operator or admin.

**Body** (application/json): [`DoctorRequest`](#doctorrequest)

**Answers**

- `200` (application/json): [`Report`](#report)
- `400` (application/json): [`ErrorBody`](#errorbody)
- `403` (application/json): [`ErrorBody`](#errorbody)

## `GET /api/v1/doctor/runs`

The connection checks kept (the last 30), newest first. Operator.

**Who may call it:** an operator or admin.

**Answers**

- `200` (application/json): list of [`DoctorRun`](#doctorrun)
- `403` (application/json): [`ErrorBody`](#errorbody)

## `GET /api/v1/doctor/runs/{id}`

One kept connection check's report, as `POST /doctor` returned it. Operator.

**Who may call it:** an operator or admin.

**Parameters**

| | In | Type | |
|---|---|---|---|
| `id` (required) | path | number | The run's id. |

**Answers**

- `200` (application/json): [`Report`](#report)
- `403` (application/json): [`ErrorBody`](#errorbody)
- `404` (application/json): [`ErrorBody`](#errorbody)

## `GET /api/v1/impact`

Which interfaces a vendor, application or host touches, how, and who owns them (for a vendor breach or a VPN/IP change). Exactly one of `vendor`, `application`, `host`.

**Who may call it:** anyone signed in (a person or a token).

**Parameters**

| | In | Type | |
|---|---|---|---|
| `vendor` | query | text | e.g. `Epic Systems` (ignores case). |
| `application` | query | text | e.g. `Sunquest Lab` (ignores case). |
| `host` | query | text | An address, subnet (`10.20.30.0/24`) or host name. |
| `resolve` | query | true or false | Look host names up in DNS (from the engine's server), so names and addresses match. |

**Answers**

- `200` (application/json): [`ImpactReport`](#impactreport)
- `400` (application/json): [`ErrorBody`](#errorbody)

## `GET /api/v1/owners`

Who owns which interface (`about.owner`), destination (`owner` on one someone else owns) and resources.yaml entry, and whether each owner is an account (username, email or display name, ignoring case, or one it had before), a team in `owners:`, or nobody the engine knows.

**Who may call it:** anyone signed in (a person or a token).

**Answers**

- `200` (application/json): list of [`OwnerGroup`](#ownergroup)

## `GET /api/v1/owners/people`

The accounts that can own things (enabled ones), for choosing a new owner: name and email.

**Who may call it:** anyone signed in (a person or a token).

**Answers**

- `200` (application/json): list of [`OwnerPerson`](#ownerperson)

## `POST /api/v1/owners/reassign/preview`

What moving ownership from one owner to another would change, as `ductus owners reassign --dry-run` shows it, plus a patch to apply in the project's repository. Writes nothing: the interfaces' files are the project's (in git), so the change is made there and deployed. Reads the files as they are on the engine's server now.

**Who may call it:** anyone signed in (a person or a token).

**Body** (application/json): [`ReassignRequest`](#reassignrequest)

**Answers**

- `200` (application/json): [`ReassignPreview`](#reassignpreview)
- `400` (application/json): [`ErrorBody`](#errorbody)

## `GET /api/v1/ports`

What the engine listens on and connects to, with TLS in and out: the same report as `ductus ports`.

**Who may call it:** anyone signed in (a person or a token).

**Answers**

- `200` (application/json): [`PortsReport`](#portsreport)

## Shapes

The JSON bodies the operations above take and return.

### `AllowedName`

| Field | Type | |
|---|---|---|
| `addresses` | list of text | The addresses let in for it: the last lookup that found some (kept when a later one fails). |
| `checked_at` (optional) | time (RFC 3339) or null | When it was last looked up. |
| `error` (optional) | text or null | Why the last lookup failed, if it did. |
| `name` | text |  |

### `CertificateInfo`

| Field | Type | |
|---|---|---|
| `alert` (optional) | number or null | Its alert (`/alerts/{id}`), when alerts are set up. |
| `chain_earliest` (optional) | text or null | Whose that is, when it isn't the server certificate's (an intermediate). |
| `chain_not_after` (optional) | time (RFC 3339) or null | The earliest expiry in its chain (what counts). |
| `checked_at` (optional) | time (RFC 3339) or null | The last check; ours: now (read from memory). |
| `days_left` (optional) | number or null | Whole days left (UTC dates), counted to the earliest expiry in the chain; negative once expired. |
| `error` (optional) | text or null | Why the last check failed, if it did; ours: why renewed files can't be used (the pair loaded before is still presented). |
| `fingerprint` (optional) | text or null | SHA-256 of the server certificate, hex. |
| `host` | text | Ours: this server's name. |
| `issuer` (optional) | text or null |  |
| `not_after` (optional) | time (RFC 3339) or null | The server certificate's own expiry. |
| `not_before` (optional) | time (RFC 3339) or null |  |
| `port` | number | Ours: 0 (see `presented`). |
| `presented` | list of text | Ours: where it is presented (`0.0.0.0:6661 MLLP over TLS (adt)`, `client certificate to hie.example.org:7443 (adt → hie)`). |
| `read_at` (optional) | time (RFC 3339) or null | When the certificate shown was read (older than `checked_at` when the last check couldn't). |
| `resource` | text | `empi` (resources.yaml), `alerts.smtp`, `alerts.<channel>`, `alerts.heartbeat`, or `<interface>/<destination>`; ours: the `certificates:` entry. |
| `side` | text | `remote` (a server the engine connects to, as its last check saw it) or `ours` (a pair the engine presents, as it loaded it). |
| `stage` (optional) | number or null | The step of the countdown it is within (a warn day; 0 once expired). |
| `state` | text | `ok`, `expiring` (within the first of `alerts.certificates.warn_days`), `expired`, or `unknown` (never read: not checked yet, or never reachable). |
| `subject` (optional) | text or null |  |
| `trusted` (optional) | true or false or null | This server trusts it (none for ours: whether senders do isn't known here). |

### `CertificatesOverview`

| Field | Type | |
|---|---|---|
| `alerting` | true or false | A rule names `certificate` (alerts are sent about them). |
| `certificates` | list of [`CertificateInfo`](#certificateinfo) | Earliest expiry first; never read ones last. |
| `last_run` (optional) | [`DoctorRun`](#doctorrun) or null |  |
| `last_scheduled` (optional) | [`DoctorRun`](#doctorrun) or null |  |
| `mode` | text | What those checks cover: `certificates` or `full`. |
| `schedule_seconds` (optional) | number or null | How often the engine checks by itself, in seconds; none when `doctor.schedule: off`. |
| `warn_days` | list of number | The steps of the countdown (`alerts.certificates.warn_days`). |

### `Check`

The result for one thing the project connects to.

| Field | Type | |
|---|---|---|
| `advice` | list of text | What a failure or warning most likely means, and what to do about it. |
| `certificate` (optional) | [`SeenCertificate`](#seencertificate) or null |  |
| `endpoint` | text | `epic.local:5000 MLLP`, a folder, `0.0.0.0:6661 MLLP`. |
| `outcome` | [`Outcome`](#outcome) | The worst step. |
| `steps` | list of [`Step`](#step) |  |
| `used_by` | list of text |  |
| `what` | text | `database patient_db`, `api empi`, `destination adt → lab`, `listener`. |

### `DocsEntry`

One documented thing and the values it has.

| Field | Type | |
|---|---|---|
| `custom` | map of name to any JSON | Values by field key, as written in `custom:`. |
| `interface` (optional) | text or null | The interface it belongs to; none for a resource. |
| `level` | text | `interface`, `destination` or `resource`. |
| `missing` | list of text | Required fields that have no value here. |
| `name` | text | `adt-lab`, `adt-lab.default`, `api empi`. |

### `DocsField`

A field the company declared in `docs-schema.yaml`.

| Field | Type | |
|---|---|---|
| `applies_to` | list of text | `interface`, `destination`, `resource`. |
| `help` (optional) | text or null |  |
| `key` | text |  |
| `required` | true or false |  |
| `title` | text | The label, else the key. |
| `type` | text | `text`, `enum`, `date`, `url`, `person` or `list`. |
| `values` | list of text | What an enum (or each item of a list) may be. |

### `DocsReport`

| Field | Type | |
|---|---|---|
| `entries` | list of [`DocsEntry`](#docsentry) |  |
| `fields` | list of [`DocsField`](#docsfield) | In the order the file lists them. |
| `problems` | list of text | What `ductus validate` would reject (wrong or undeclared values, missing required ones). |

### `DoctorRequest`

| Field | Type | |
|---|---|---|
| `interfaces` (optional) | list of text | Only what these interfaces use (the engine's database is always checked). |
| `mode` (optional) | [`DoctorRunMode`](#doctorrunmode) | `full` (the default: logins included) or `certificates` (DNS, TCP and the TLS handshake of what has a certificate; nothing logs in). |
| `timeout_ms` (optional) | number or null | Longest wait per step, in milliseconds (default 5000, at most 60000). |

### `DoctorRun`

| Field | Type | |
|---|---|---|
| `at` | time (RFC 3339) |  |
| `checks` | number |  |
| `failed` | number |  |
| `id` | number |  |
| `mode` | text | `certificates` or `full`. |
| `seconds` | number |  |
| `started_by` | text | `schedule`, or who asked. |

### `DoctorRunMode`

`full`, `certificates`

### `ErrorBody`

| Field | Type | |
|---|---|---|
| `error` | text | What went wrong, in plain words. |

### `ImpactHit`

| Field | Type | |
|---|---|---|
| `endpoints` | list of text | Hosts and ports, listening ports, folders to look at. |
| `interface` | text |  |
| `owner` (optional) | text or null |  |
| `reason` | text | Why it matched. |
| `role` | text | `source`, `destination lab`, `database patient_db`, `api empi`. |

### `ImpactReport`

| Field | Type | |
|---|---|---|
| `affected` | number | Interfaces affected, of `interfaces`. |
| `hits` | list of [`ImpactHit`](#impacthit) |  |
| `incomplete` | list of text | Interfaces that don't name every system they connect (they may be missing here). |
| `interfaces` | number |  |
| `known` | list of text | With no hits for a vendor or application: the names that are known. |
| `query` | text | What was looked for, e.g. `vendor "Epic Systems"`. |

### `ListeningPort`

| Field | Type | |
|---|---|---|
| `allow` | text | Who may connect: the `allow:` list (`10.1.20.0/24, epic-bridge01`), or `anyone`. |
| `allow_detail` (optional) | [`SourceAllow`](#sourceallow) or null |  |
| `host` | text |  |
| `interface` | text | The interface, or `(engine REST API)`. |
| `port` | number |  |
| `protocol` | text | `MLLP`, `MLLP over TLS`, `HTTP` or `HTTPS`. |
| `route` (optional) | text or null | HTTP only: `GET /patients/{mrn} (respond)`. |
| `tls` | text | `none`, or what it presents and asks for (`presents ductus-mllp, clients need a certificate from hospital-ca`). |

### `OutboundPort`

| Field | Type | |
|---|---|---|
| `host` | text |  |
| `port` | number |  |
| `protocol` | text | `MLLP`, `MLLP over TLS`, `HTTP`, `HTTPS`, `SFTP`, `LDAP`, `LDAPS`, `Postgres`, `SQL Server`, `SQL Browser (UDP)`. |
| `tls` | text | For MLLP, HTTP and directories: `none`, `TLS` (the platform's trusted certificates), or TLS with what it trusts and presents (`TLS: hie-ca trusted, client certificate ductus-client`). Empty for SFTP (SSH) and databases. |
| `used_by` | text | The interface (`adt → lab`), or `resources.yaml: <name>`. |

### `Outcome`

`ok`, `warn`, `fail`

### `Owned`

| Field | Type | |
|---|---|---|
| `destination` (optional) | text or null | Set for a destination owned by someone other than its interface's owner. |
| `interface` | text |  |

### `OwnedResource`

| Field | Type | |
|---|---|---|
| `kind` | text | `database`, `api`, `sftp server` or `directory`. |
| `name` | text |  |

### `OwnerGroup`

| Field | Type | |
|---|---|---|
| `advice` (optional) | text or null | How the owner should be written instead, when it reaches someone but isn't written as their email (renamed, reused, a username or display name), in words. |
| `owner` (optional) | text or null | Absent for what nobody owns (listed last). |
| `owns` | list of [`Owned`](#owned) | Interfaces, and destinations owned by someone other than their interface's owner. |
| `person` (optional) | [`OwnerPerson`](#ownerperson) or null |  |
| `reach` (optional) | text or null | Who the owner is: `user` (an account, in `person`), `renamed` (an account by a name it had before, which nobody has now: in `person`, the former name in `was`), `reused` (the account that has the name now, in `person`; another had it before, in `was`), `disabled` (an account that is disabled or refused by the directory, in `person`), `several` (names more than one account: none is chosen), `team` (in `owners:` in ductus.yaml), `address` (an email address, not an account) or `nobody`. Absent for the unowned. `renamed` and `reused` still reach `person` (alerts too); `disabled`, `several` and `nobody` reach no one. |
| `resources` | list of [`OwnedResource`](#ownedresource) | resources.yaml entries with this `owner`. |
| `suggest` (optional) | text or null | What to write instead (the account's email), for the move's `to`. |
| `was` (optional) | [`OwnerWas`](#ownerwas) or null |  |
| `you` | true or false | The owner is the caller (or the person whose token calls). |

### `OwnerPerson`

An account, as an owner.

| Field | Type | |
|---|---|---|
| `disabled` | true or false | Disabled, or refused by the directory. |
| `display_name` (optional) | text or null |  |
| `email` (optional) | text or null |  |
| `username` | text |  |

### `OwnerWas`

A name an account had before.

| Field | Type | |
|---|---|---|
| `kind` | text | `username`, `email` or `display_name`. |
| `until` | time (RFC 3339) | When it stopped being theirs. |
| `value` | text |  |
| `whose` | text | The account that had it (its username now). |

### `PortsReport`

| Field | Type | |
|---|---|---|
| `connects_to` | list of [`OutboundPort`](#outboundport) | What it connects to (destinations and resources.yaml), by host: what a firewall request lists. |
| `listening` | list of [`ListeningPort`](#listeningport) | What the engine listens on (its interfaces and the API), by port. |

### `ReassignFile`

| Field | Type | |
|---|---|---|
| `interface` (optional) | text or null | The interface whose interface.yaml this is; absent for resources.yaml. |
| `lines` | list of [`ReassignLine`](#reassignline) |  |
| `path` | text | Relative to the project, `/`-separated. |
| `resources` | list of text | resources.yaml entries whose owner changes. |

### `ReassignLine`

| Field | Type | |
|---|---|---|
| `after` | text |  |
| `before` | text |  |
| `line` | number | 1-based. |

### `ReassignPreview`

| Field | Type | |
|---|---|---|
| `changed` | number | Owner values that would change, over all files. |
| `command` | text | The same change made with the CLI in the project's folder. |
| `files` | list of [`ReassignFile`](#reassignfile) | Files that would change, each with its changed lines. |
| `known` | list of text | The owners there are, when nothing matched `from`. |
| `patch` | text | The whole change as a unified diff, for `git apply` in the project's repository. |
| `refused` | list of [`ReassignRefused`](#reassignrefused) | Files with `owner: <from>` that can't be changed safely by a program (an unusual layout): change them by hand. While any is listed, `patch` and `command` are empty. |
| `to_person` (optional) | [`OwnerPerson`](#ownerperson) or null |  |
| `to_reach` | text | The new owner, as `reach` in `/owners` says it. |
| `warning` (optional) | text or null | Said when the new owner isn't an account (or names a disabled one, or several). |

### `ReassignRefused`

| Field | Type | |
|---|---|---|
| `error` | text |  |
| `interface` (optional) | text or null | Absent for resources.yaml. |
| `path` | text |  |

### `ReassignRequest`

| Field | Type | |
|---|---|---|
| `from` | text | The current owner (ignores case). |
| `interfaces` (optional) | list of text | Only these interfaces. With neither `interfaces` nor `resources`, everything moves. |
| `resources` (optional) | list of text | Only these resources.yaml entries (by name). |
| `to` | text | The new owner. |

### `Report`

The whole report: what `ductus doctor` prints and `--report` saves.

| Field | Type | |
|---|---|---|
| `checks` | list of [`Check`](#check) |  |
| `environment` | text |  |
| `generated_at` | text |  |
| `host` | text |  |
| `notes` (optional) | list of text | Lines about this machine said before the checks (on Windows: the service's account). |
| `seconds` | number |  |
| `version` | text |  |

### `SeenCertificate`

A server's certificate as a check saw it.

| Field | Type | |
|---|---|---|
| `chain_earliest` (optional) | text or null | Whose that is, when it isn't the server certificate's (an intermediate). |
| `chain_not_after` (optional) | time (RFC 3339) or null | The earliest expiry in the chain the server presented. |
| `error` (optional) | text or null | Why the check failed: couldn't connect, not trusted, expired. |
| `fingerprint` (optional) | text or null | SHA-256 of the server certificate (DER), hex. |
| `host` | text |  |
| `issuer` (optional) | text or null |  |
| `not_after` (optional) | time (RFC 3339) or null |  |
| `not_before` (optional) | time (RFC 3339) or null |  |
| `port` | number |  |
| `resource` | text | What it belongs to: `empi` (resources.yaml), `alerts.smtp`, `alerts.<channel>`, `alerts.heartbeat`, `<interface>/<destination>`. |
| `subject` (optional) | text or null | The server certificate's common name (or organisation); none if none was read. |
| `trusted` | true or false | This server trusts it (the system's trusted certificates). |

### `SourceAllow`

A listener's `allow:` list and how it's doing.

| Field | Type | |
|---|---|---|
| `entries` | list of text | The list as written: addresses, subnets and host names. |
| `last_refused` (optional) | text or null | The last refused connection's address and time. |
| `last_refused_at` (optional) | time (RFC 3339) or null |  |
| `names` | list of [`AllowedName`](#allowedname) | Each host name in it with what it resolves to now. |
| `refused` | number | Connections refused since the engine started (0 when this engine doesn't hold the listener). |

### `Step`

One step of a check: `DNS`, `connect`, `TLS`, `login`, `HTTP`, `commits`, `folder`, `port`.

| Field | Type | |
|---|---|---|
| `detail` | text |  |
| `name` | text |  |
| `outcome` | [`Outcome`](#outcome) |  |

