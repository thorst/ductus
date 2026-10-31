# resources.yaml: what interfaces share

`resources.yaml` defines, once and by a name, everything interfaces connect to that isn't a
source or a destination of their own: the databases their code queries, SFTP servers, HTTP APIs,
directories people sign in with, and certificates. Interfaces and their code use the name:

```ts
const row = await db("patient_db").one("SELECT mrn FROM patients WHERE id = $1", [id]);
const patient = await api("empi").get(`/patients/${mrn}`);
```

**Why names:** no address, login or password is ever written in an interface or its code. The
code is the same in every environment; `resources.yaml` fills in each environment's host with
`${NAME}` and its password with `${secret:NAME}` ([environments](environments.md)). The engine
logs in on the code's behalf, so the code never sees a credential. And because every connection
is named, `ductus doctor` can check each one from the server, `ductus impact` can say which
interfaces use it, and its certificate is watched for expiry.

**When not:** an interface's own source and destinations (an MLLP receiver, a folder) are in its
interface.yaml, not here, unless several share them: an SFTP server and an HTTP API are always
resources, so their login is in one place.

Every entry of every kind also takes `application`, `vendor` and `owner` (who to ask, best an
email address: its certificate's expiry alerts reach them with `to: owner`), for `ductus impact`
and alerts, and `custom:`, your own [documentation fields](documentation-fields.md).

## Databases

[`databases`](../reference/config/resources.md#databases) are the databases interfaces' code
queries with `db("<name>")`: lookups, inserts, a nightly extract. Each is
[`postgres`](../reference/config/resources.md#databasesnamepostgres) or
[`sqlserver`](../reference/config/resources.md#databasesnamesqlserver).

```yaml
# resources.yaml
databases:
  patient_db:
    postgres:
      host: ${PATIENT_DB_HOST}
      database: patients
      user: ductus_lookup
      password: ${secret:PATIENT_DB_PASSWORD}
      tls: verify-full
      max_connections: 5
      application: Patient index
      owner: dba@hospital.org
  billing_db:
    sqlserver:
      host: ${BILLING_DB_HOST}
      database: Billing
      user: ductus
      password: ${secret:BILLING_DB_PASSWORD}
      application: Billing
      vendor: Cerner
```

- **The login**: give it only what the queries need, often read-only on a few tables or views.
  A wrong password makes messages wait and retry rather than quarantine: fixing it fixes them
  all ([nuance](../nuances.md#database-login-failures-make-messages-wait-not-fail-level-3)).
- **Connections**: each entry has a pool, shared by every interface's calls, of up to
  `max_connections` (default 5), opened as needed. A call that finds them all busy waits up to
  10 s, then fails as a temporary error (the message waits and is tried again). Raise it when
  calls queue while the database itself is quick, within what its DBA allows for every engine;
  don't raise it to cure slow queries. A busy interface with `isolation: dedicated` can have
  connections of its own ([`dedicated_pools`](delivery.md#dedicated-threads-and-connections)).
- **Postgres TLS** works as for the engine's own database: `tls: verify-full` checks the server's
  certificate against this server's trusted CAs (`ca:` names a private one); without `tls`, it's
  `prefer`, which checks nothing ([PostgreSQL's TLS](../install/postgres.md#tls-to-the-database)).
- **SQL Server is always TLS, verified**: `encrypt: mandatory` by default, `strict` (TDS 8) for
  servers set to force it; there's no unencrypted mode. `ca:` for the domain's CA where this
  server doesn't trust it; `name_in_certificate` when connecting by an alias or a listener's
  name ([nuance](../nuances.md#sql-server-connections-are-verified-and-trust_server_certificate-is-for-labs-level-3)).
  A named instance is `instance: PROD`: its port comes from the SQL Server Browser (UDP 1434,
  which the firewall must let through).
- **SQL Server Windows logins** (`auth: windows`), where the DBA allows only those: without
  `user`, the engine logs in as the Windows service's own account (a gMSA is best: no password
  anywhere); with `user` and `password`, as that domain account. From Linux that is always NTLM,
  which some servers refuse ([nuance](../nuances.md#auth-windows-the-engines-own-account-or-a-named-one-level-1),
  [from Linux](../nuances.md#from-linux-a-windows-login-is-always-ntlm-level-3)).

```yaml
# resources.yaml
databases:
  billing_db:
    sqlserver:
      host: ${BILLING_DB_HOST}
      instance: PROD
      database: Billing
      auth: windows          # as the service's account: on Windows, best a gMSA
```

**What code can do with one**, and what to watch for: one result set per call on SQL Server, a
query that writes is a write a crash can repeat, write names out (`db("patient_db")`, not
`db(name)`, so `ductus impact` can see it)
([databases and APIs](../nuances.md#databases-and-apis), [running twice](../nuances.md#your-transform-can-run-more-than-once-for-the-same-message-level-3)).

## SFTP servers

[`sftp`](../reference/config/resources.md#sftp) entries are the servers `sftp` sources read from
and `sftp` destinations write to (`server: <name>`).

```yaml
# resources.yaml
sftp:
  quest:
    host: ${QUEST_SFTP_HOST}
    user: hospital
    private_key: ${secret:QUEST_SFTP_KEY}
    host_key: "SHA256:nThbg6kXUpJWGl7E1IGOCspRomTxdCARLviKw6E5SY8"
    application: Quest Lab Results
    vendor: Quest
```

- **Log in with a key** where the server takes one: `ssh-keygen -t ed25519 -f quest` makes a
  pair; the private half goes in a secret (`ductus secret set QUEST_SFTP_KEY --env prod <quest`),
  the public half (`quest.pub`) to the server's owner. A `password` works too.
- **`host_key`** is the server's key fingerprint, so a server pretending to be this one is
  refused before any password or key reaches it. Leave it out the first time: the connection
  fails, and the error gives the fingerprint the server presented. Check it with the server's
  administrator, then paste it in. When their key changes (a rebuilt server), connections fail
  until you update it: on purpose, since that's what an impostor looks like too.

## HTTP APIs

[`apis`](../reference/config/resources.md#apis) are the HTTP APIs the code calls with
`api("<name>")`, and that `http` destinations send through (`api: <name>`): a base URL, the
headers every request carries (the token), a timeout, and TLS.

```yaml
# resources.yaml
apis:
  empi:
    url: ${EMPI_URL}
    headers: { Authorization: "Bearer ${secret:EMPI_TOKEN}" }
    timeout: 10s
    application: EMPI
    owner: empi-team@hospital.org
```

- Paths in code and destinations go under the URL: `api("empi").get("/patients/1")` calls
  `${EMPI_URL}/patients/1`.
- A header the code or a destination sets itself replaces the API's of the same name
  ([nuance](../nuances.md#api-headers-the-apis-then-yours-level-2)).
- 5xx, 408 and 429 answers are retried by default
  ([nuance](../nuances.md#fetch-and-api-retry-5xx-408-and-429-by-default-level-3)).
- `ca:` for an API with a private CA's certificate, `certificate:` for one that asks for a client
  certificate (mutual TLS): both `certificates:` entries ([TLS](tls.md)).

## Directories

[`ldap`](../reference/config/resources.md#ldap) entries are the directories people sign in to
the web UI with (Active Directory, OpenLDAP): the server, its TLS and the service account that
looks people up. `auth.ldap` in ductus.yaml names the one to use and maps its groups to roles:
[People: users, tokens, the directory](people.md#signing-in-with-the-directory).

## Certificates

[`certificates`](../reference/config/resources.md#certificates) are the certificates the engine
presents (on its listeners, the API, and as a client certificate) and the CAs it trusts or
checks clients against, each by a name: [TLS and certificates](tls.md).
