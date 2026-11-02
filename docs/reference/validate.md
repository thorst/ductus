# Validate findings

What `ductus validate` reports, by code. The text output prints each finding on a line of its own (`✗` an error, `⚠` a warning, `note:` what wasn't checked); `ductus validate --json` gives the same findings to tools, with the code, the file and the line and column of the key or code it's about ([testing guide](../write/testing.md#for-editors-and-ci-json)). Errors make the project invalid (exit 1); warnings and notes don't.

Codes are stable: a check keeps its code, a new check gets a new one, and a retired code isn't reused. Three families come from elsewhere:

- **`TS` followed by a number** (`TS2322`): TypeScript's own errors, with `source: typescript` in the JSON. VS Code's TypeScript server shows them as you type; `validate` checks with the TypeScript version it carries.
- **`DX1` to `DX4`**: Ductus's checks of interface code: writing to the global scope (`DX1`), changing built-in prototypes (`DX2`), mutable state in a shared library (`DX3`) ([the shared pool](../shared-pool.md)), and a database or API name computed at run time (`DX4`) ([lookups](../write/lookups.md)).
- **`DUC` followed by three digits**: everything else, below. For the five files, the last digit says how far loading got: 0 not YAML, 1 a `${...}` value the environment doesn't have, 2 a setting that doesn't exist or has the wrong kind of value, 3 a rule between settings, 4 the file can't be read.

| Code | Usually | What it means, and what to do |
|---|---|---|
| `DUC100` | error | **An environment file isn't YAML.** `environments/<env>.yaml` doesn't parse: the line and column are where the parser stopped. Usually an indentation or a missing quote. |
| `DUC102` | error | **An environment file has a setting it doesn't know.** `environments/<env>.yaml` has a key other than `variables`, or a value of the wrong kind. Put every value under `variables:`. |
| `DUC104` | error | **The environment can't be loaded.** `--env` names an environment with no file in `environments/`, or its secrets (`secrets/<env>.json`) can't be read. Check the name, or create the file. |
| `DUC110` | error | **resources.yaml isn't YAML.** It doesn't parse: the line and column are where the parser stopped. |
| `DUC111` | error | **resources.yaml uses a value the environment doesn't have.** A `${NAME}` or `${secret:NAME}` has no value in this environment: the place is the first one missing; the message lists them all. Add it to `environments/<env>.yaml`, or set the secret with `ductus secret set NAME --env <env>`. |
| `DUC112` | error | **resources.yaml has a setting it doesn't know, or a value of the wrong kind.** The place is the setting (an unknown key itself). The reference's resources.yaml page lists every setting. |
| `DUC113` | error | **A resources.yaml entry breaks a rule.** An entry that parses but can't work as written (an SFTP server without a password or key, an API `url` that isn't http(s), a certificate without its key): the place is the entry. |
| `DUC114` | error | **resources.yaml can't be read.** The file exists but can't be read (permissions). |
| `DUC115` | warning | **A certificate's path has no drive letter (Windows).** Windows reads it from the project's drive. Write the whole path, or one relative to the project folder. |
| `DUC116` | warning | **A directory is reached with plain ldap://.** Passwords cross the network readable. Use ldaps:// or `starttls: true` outside a test directory. |
| `DUC117` | warning | **A Postgres database's certificate isn't checked.** Whoever answers at its address is believed. Set `tls: verify-full` (with `ca:` for a private CA). |
| `DUC118` | warning | **A SQL Server's certificate isn't checked.** `trust_server_certificate: true`: whoever answers at its address is believed. Remove it, with `ca:` for a private CA. |
| `DUC119` | warning | **A Windows login to SQL Server from Linux uses NTLM.** Never Kerberos from Linux: Microsoft is retiring NTLM and some domains block it. A SQL login, or the engine on Windows, avoids it. |
| `DUC120` | error | **An interface.yaml isn't YAML.** It doesn't parse: the line and column are where the parser stopped. |
| `DUC121` | error | **An interface.yaml uses a value the environment doesn't have.** A `${NAME}` or `${secret:NAME}` has no value in this environment: the place is the first one missing. Add it to `environments/<env>.yaml` (or the secret). |
| `DUC122` | error | **An interface.yaml has a setting it doesn't know, or a value of the wrong kind.** The place is the setting (an unknown key itself, a port of 70000). |
| `DUC123` | error | **An interface.yaml breaks a rule.** Settings that parse but don't go together, or a value out of range (`workers` without `isolation: dedicated`, a destination name with capitals): the place is the setting the message names. |
| `DUC124` | error | **An interface.yaml can't be read.** The file exists but can't be read (permissions). |
| `DUC125` | error | **An interface's code doesn't load.** Its `script` doesn't load the way the engine loads it, or doesn't export a function a destination names (`transform: toLab`): the place is the `transform` (or `script`) that names it. |
| `DUC130` | error | **ductus.yaml isn't YAML.** It doesn't parse: the line and column are where the parser stopped. |
| `DUC131` | error | **ductus.yaml's alerts use a value the environment doesn't have.** A `${NAME}` or `${secret:NAME}` under `alerts:` has no value in this environment. |
| `DUC132` | error | **ductus.yaml has a setting it doesn't know, or a value of the wrong kind.** The place is the setting. |
| `DUC133` | error | **ductus.yaml breaks a rule.** A setting out of range (`audit.keep` under a year, `isolates.heap_mb` under 64) or alerts that can't work as written. |
| `DUC134` | error | **ductus.yaml can't be read.** The file exists but can't be read (permissions). |
| `DUC150` | warning | **The Windows service's account can't log in where the project needs it.** Shown on Windows only: the service's account and the SQL Server logins that use it (`ductus service account`). |
| `DUC200` | error | **Interfaces want the same port.** Two MLLP listeners on one port, MLLP and HTTP on one port, two interfaces handling the same HTTP method and path, or HTTP interfaces on one port with a different `tls` or `allow`. The place is the second interface's source. |
| `DUC201` | error | **A route between interfaces can't work.** A route (`destination: { interface: b }`) to an interface that doesn't exist or doesn't have `source: internal`, in a format the target doesn't read, or in a loop. |
| `DUC202` | error | **An interface names a resource resources.yaml doesn't have.** An SFTP server, database or API named in interface.yaml: add it to resources.yaml, or fix the name (the message lists those defined). |
| `DUC203` | error | **Code names a resource resources.yaml doesn't have.** `db("name")` or `api("name")` in TypeScript: the place is the call. |
| `DUC204` | warning | **An internal interface gets nothing.** It has `source: internal` but no interface routes to it. |
| `DUC205` | warning | **An interface doesn't say which systems it connects.** Name the sending and receiving application or vendor (`about.from`, `about.to`, or on the destination), so `ductus impact` can find it. An error with `require_about: true` in ductus.yaml. |
| `DUC210` | error | **A TLS setting names the wrong certificate.** A listener's `tls.certificate`, or a connection's `ca` or `certificate`, names an entry `certificates:` doesn't have, or one of the wrong kind (a CA where a pair is needed, or the other way round). |
| `DUC211` | error | **A certificate doesn't load.** A `certificates:` entry's files can't be read, don't match their key, or have expired: the engine would refuse to start. |
| `DUC212` | warning | **A certificate expires within 30 days.** Renew it: the engine alerts too, 30, 14 and 7 days before. |
| `DUC213` | warning | **The REST API is on the network without TLS.** Passwords and tokens cross it readable, and browsers won't keep the session cookie: add `tls: { certificate: <name> }` to `api`. |
| `DUC220` | warning | **An `allow` list names a host that doesn't resolve.** Nothing is let in for it until it resolves (the engine looks again every 5 minutes). DNS seen from here may not be the engine's. |
| `DUC221` | warning | **A sender `about` names isn't allowed.** `about.from.hosts` names a host the listener's `allow` would refuse. |
| `DUC222` | warning | **Can't tell whether a sender is allowed.** `about.from.hosts` names a host that doesn't resolve from here. |
| `DUC223` | warning | **api.allow leaves out this machine.** `ductus status`, `drain` and the web UI on this machine would be refused: add 127.0.0.1 if you use them here. |
| `DUC230` | error | **Directory sign-in can't be set up.** `auth.ldap` names a directory resources.yaml doesn't have, or one that can't be used as written. |
| `DUC231` | warning | **Directory sign-in can't tell people apart.** Settings under which a renamed person and a new one given a departed person's name look the same (`attributes.id: none`, a `user_filter` that doesn't match one attribute). |
| `DUC240` | warning | **alert.to is set, but there are no alerts.** ductus.yaml has no `alerts:`, so nothing is sent: add them, or remove `alert.to`. |
| `DUC241` | warning | **Alerts that may not work as meant.** What `alerts:` would do that's probably a mistake (a rule nothing reaches, a channel no rule uses). |
| `DUC242` | error | **alert.to names a channel alerts don't have.** Add the channel under `alerts.channels`, or fix the name. |
| `DUC243` | warning | **An owner alerts can't reach.** Alerts go to `owner`, but the owner isn't an email address or a team in `owners:` (without a database, accounts can't be checked). |
| `DUC244` | warning | **An owner doesn't match one account.** Checked against the database's accounts: renamed, disabled, several accounts, or nobody. The message gives the fix (`ductus owners reassign`). |
| `DUC250` | error | **docs-schema.yaml doesn't load.** It doesn't parse, or declares a field it can't use. |
| `DUC251` | error | **A documentation field's value is wrong or missing.** A `custom:` value isn't declared in docs-schema.yaml, isn't one of its values, or a required one is missing. |
| `DUC300` | error | **The database can't be checked.** `--database-url` (or DUCTUS_DATABASE_URL) was given, but it can't be connected to or read. |
| `DUC301` | warning | **The database connection isn't verified.** Whoever answers at its address is believed: add `sslmode=verify-full` to the URL (`sslrootcert=/path/ca.pem` for a private CA). |
| `DUC302` | warning | **Couldn't read how the database connection is protected.** The check of the connection's TLS failed; the rest went on. |
| `DUC303` | error | **Lanes would strand waiting messages.** `ordering.partitions` is lower than the lanes messages wait in: the engine would refuse to start. Keep it as it is until they're sent (`ductus drain --wait`). |
| `DUC304` | error | **The lanes couldn't be checked.** Reading the waiting messages' lanes failed. |
| `DUC305` | warning | **More lanes with messages waiting.** A patient's (key's) new messages may be sent before its older ones until those are. |
| `DUC306` | warning | **A retention change is still running.** Its messages are moving in the background. |
| `DUC307` | warning | **A shorter retention waits for a confirmation.** It deletes messages, so it waits for `ductus retention confirm <interface>`. |
| `DUC308` | warning | **Retention changes at the next start.** Its messages move in the background then. |
| `DUC309` | warning | **Retention couldn't be read.** Reading the database's retention state failed; the rest went on. |
| `DUC310` | warning | **The accounts couldn't be read.** Owners weren't matched to accounts. |
| `DUC390` | note | **Not checked against a database.** Without `--database-url` (or DUCTUS_DATABASE_URL), lanes that would strand waiting messages and retention changes aren't checked. |
| `DUC391` | note | **This version hasn't run on the database yet.** The lanes interfaces last ran with aren't recorded, so raised lanes can't be told (lowered ones are checked). |
