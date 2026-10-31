# `resources.yaml`

`resources.yaml`: what many interfaces share, each defined once by a name: databases, SFTP servers, HTTP APIs, directories for signing in, and certificates.

Interfaces and their code refer to them by name (`db("patient_db")`, `api("empi")`, `server: lab_server`), so no address or credential is ever written in an interface or its code, and each environment fills in its own with `${NAME}` and `${secret:NAME}`. `ductus doctor` checks every entry from the server, and `ductus impact` finds the interfaces behind each one.

Schema for editors: `docs/schema/resources.json` (written into a project by `ductus types`). Files: `resources.yaml`.

### `databases`

**Type:** names, each with settings, below

Databases the interfaces' code queries with `db("<name>")`: each a `postgres` or a `sqlserver` entry, by a name you choose. The engine keeps a pool of connections to each and logs in with the entry's credentials, so code never sees them. Default: none. Example: `databases: { patient_db: { postgres: { host: db1, database: patients, user: ductus, password: "${secret:PATIENT_DB_PASSWORD}" } } }`.

### `databases.<name>.postgres`

**Type:** settings, below

A PostgreSQL database. Example: `postgres: { host: db1.hospital.local, database: patients, user: ductus, password: "${secret:PATIENT_DB_PASSWORD}", tls: verify-full }`.

### `databases.<name>.postgres.host`

**Type:** text · **Required**

The database server's host name or address. With `tls: verify-full`, its certificate must be for this name. Usually from the environment (`${PATIENT_DB_HOST}`). Required. Example: `host: db1.hospital.local`.

### `databases.<name>.postgres.port`

**Type:** number

The server's port. Default: `5432`. Example: `port: 5433`.

### `databases.<name>.postgres.database`

**Type:** text · **Required**

The database to connect to on that server. Required. Example: `database: patients`.

### `databases.<name>.postgres.user`

**Type:** text · **Required**

The login. Give it only what the interfaces' queries need (often read-only on a few tables or views). Required. Example: `user: ductus`.

### `databases.<name>.postgres.password`

**Type:** text

The login's password, from a secret (`${secret:PATIENT_DB_PASSWORD}`). A wrong one makes messages wait and retry rather than quarantine, since fixing it fixes them all. Default: none (for `certificate` or trust logins). Example: `password: "${secret:PATIENT_DB_PASSWORD}"`.

### `databases.<name>.postgres.max_connections`

**Type:** number

The most connections the engine opens to this database, shared by every interface's `db("<name>")` calls, opened as they're needed.

A call that finds them all busy waits up to 10 seconds for one, then fails as a temporary error: its message waits and is tried again, nothing is quarantined. Raise it when calls queue for a connection while the database itself is quick, up to what the database's own limit and its DBA allow for every engine that connects (each engine has its own pool). Don't raise it to cure slow queries: more of the same slow query at once makes the database slower. An interface with `isolation: dedicated` can have connections of its own instead (`dedicated_pools`), so one busy interface can't take them all. Default: `5`. Example: `max_connections: 10`.

### `databases.<name>.postgres.tls`

**Type:** `disable`, `prefer`, `require`, `verify-ca`, `verify-full`

How the connection is protected, with the names and meanings of libpq's `sslmode`, as DBAs write them:

- `disable`: no TLS.
- `prefer`: TLS when the server offers it, its certificate not checked (anyone answering at that address is believed).
- `require`: TLS or no connection, the certificate not checked.
- `verify-ca`: TLS, the certificate issued by a trusted CA; the name in it not checked.
- `verify-full`: TLS, a trusted CA's certificate for `host`. The only one that stops a server pretending to be this one.

Use `verify-full` for any database on another machine (cloud Postgres requires TLS), with `ca` for a private CA. The engine's start, `ductus validate` and `ductus doctor` say how each connection is protected. Default: `verify-full` when `ca` is given, else `prefer`. Example: `tls: verify-full`.

### `databases.<name>.postgres.ca`

**Type:** text

A CA to trust for this server besides the ones this server trusts (a `certificates:` entry), for a database whose certificate comes from a private CA. Naming one means `tls: verify-full` unless `tls` says otherwise. Default: the system's trusted CAs only. Example: `ca: hospital-ca`.

### `databases.<name>.postgres.certificate`

**Type:** text

A client certificate to present (a `certificates:` entry with its key): for a login by certificate (`cert` in the server's pg_hba.conf), or alongside the password where the DBA asks for both. A renewed certificate is used by new connections without a restart. Default: none. Example: `certificate: ductus-db`.

### `databases.<name>.postgres.application`

**Type:** text

The application whose database this is, for `ductus impact`. Default: none. Example: `application: Patient index`.

### `databases.<name>.postgres.vendor`

**Type:** text

The database's (or application's) vendor, for `ductus impact`. Default: none. Example: `vendor: In-house`.

### `databases.<name>.postgres.owner`

**Type:** text

Who to ask about this database, best as an email address (its certificate's expiry alerts reach them with `to: owner`). Default: none. Example: `owner: dba@hospital.org`.

### `databases.<name>.postgres.custom`

**Type:** names and values of your own

Your company's documentation fields for this entry (`applies_to: [resource]` in `docs-schema.yaml`). Default: none. Example: `custom: { ticket: CHG-1234 }`.

### `databases.<name>.sqlserver`

**Type:** settings, below

A Microsoft SQL Server database, with a SQL login or a Windows one. Always over TLS, its certificate checked. Example: `sqlserver: { host: billing.hospital.local, database: Billing, user: ductus, password: "${secret:BILLING_DB_PASSWORD}" }`.

### `databases.<name>.sqlserver.host`

**Type:** text · **Required**

The SQL Server's host name or address (without `\INSTANCE`: that goes in `instance`). Its certificate must be for this name, unless `name_in_certificate` says another. Required. Example: `host: billing.hospital.local`.

### `databases.<name>.sqlserver.port`

**Type:** number

The server's port. Default: `1433`, or, with `instance`, the port the SQL Server Browser gives for it (UDP 1434: the firewall must let that through too). Example: `port: 1435`.

### `databases.<name>.sqlserver.instance`

**Type:** text

A named instance, as SQL Server tools write `host\INSTANCE`: `SQLEXPRESS`, `PROD`. Its port is asked of the SQL Server Browser, unless `port` is given. Default: the default instance. Example: `instance: PROD`.

### `databases.<name>.sqlserver.database`

**Type:** text · **Required**

The database to use on that server. Required. Example: `database: Billing`.

### `databases.<name>.sqlserver.auth`

**Type:** `sql`, `windows`

How the engine logs in:

- `sql`: a SQL Server login, `user` and `password`.
- `windows`: a Windows login. Without `user`, as the account the engine runs as (the Windows service's account; Windows only, Kerberos where the domain allows it); with `user` and `password`, as that domain account (from Linux this is NTLM, which some servers refuse: Extended Protection set to Required).

Use `windows` where the DBA allows only Windows logins; on Windows prefer the service's own account (a gMSA), which has no password to keep. Default: `sql`. Example: `auth: windows`.

### `databases.<name>.sqlserver.user`

**Type:** text

The login: a SQL login's name with `auth: sql`, or a domain account (`HOSPITAL\svc-lab`) with `auth: windows`, where leaving it out means the account the engine runs as. Give it only what the queries need. Example: `user: ductus`.

### `databases.<name>.sqlserver.password`

**Type:** text

The login's password, from a secret (`${secret:BILLING_DB_PASSWORD}`). Not with `auth: windows` and no `user`. Example: `password: "${secret:BILLING_DB_PASSWORD}"`.

### `databases.<name>.sqlserver.max_connections`

**Type:** number

The most connections the engine opens to this database, shared by every interface's `db("<name>")` calls, opened as they're needed. A call that finds them all busy waits for one, then fails as a temporary error (the message waits and is tried again). Raise it when calls queue while the server is quick, within what its DBA allows for every engine. Default: `5`. Example: `max_connections: 10`.

### `databases.<name>.sqlserver.ca`

**Type:** text

A CA to trust for this server besides the ones this server trusts (a `certificates:` entry), for a SQL Server whose certificate comes from a private CA (common: the domain's CA, or a certificate the DBA made). Default: the system's trusted CAs only. Example: `ca: hospital-ca`.

### `databases.<name>.sqlserver.encrypt`

**Type:** mandatory or strict

How the connection is encrypted, always with the server's certificate checked:

- `mandatory`: TLS for the whole session, begun after SQL Server's first exchange, as every SQL Server client does it.
- `strict`: TDS 8, TLS before anything else, for servers set to force it (SQL Server 2022 and later; 2022 on Linux refuses it).

There's no `false`: SQL Server would encrypt the login only and send queries and results readable. Default: `mandatory`. Example: `encrypt: strict`.

### `databases.<name>.sqlserver.name_in_certificate`

**Type:** text

The name the server's certificate is checked for, when it isn't `host`: connecting to an availability group listener, an alias or an address whose certificate names the server itself (ADO.NET's HostNameInCertificate). Default: `host`. Example: `name_in_certificate: sql1.hospital.org`.

### `databases.<name>.sqlserver.trust_server_certificate`

**Type:** `true` or `false`

Accept the server's certificate without checking it: anyone answering at its address would be believed, and could read the credentials and data. For a lab server only; the start and `ductus validate` warn about it on another machine. For a server with a private CA, name the CA in `ca` instead. Not with `ca` or `name_in_certificate`. Default: `false`. Example: `trust_server_certificate: true`.

### `databases.<name>.sqlserver.application`

**Type:** text

The application whose database this is, for `ductus impact`. Default: none. Example: `application: Billing`.

### `databases.<name>.sqlserver.vendor`

**Type:** text

The application's vendor, for `ductus impact`. Default: none. Example: `vendor: Cerner`.

### `databases.<name>.sqlserver.owner`

**Type:** text

Who to ask about this database, best as an email address (its certificate's expiry alerts reach them with `to: owner`). Default: none. Example: `owner: dba@hospital.org`.

### `databases.<name>.sqlserver.custom`

**Type:** names and values of your own

Your company's documentation fields for this entry (`applies_to: [resource]` in `docs-schema.yaml`). Default: none. Example: `custom: { ticket: CHG-1234 }`.

### `sftp`

**Type:** names, each with settings, below

SFTP servers that `sftp` sources read from and `sftp` destinations write to (`server: <name>`), with their login and host key. Default: none. Example: `sftp: { lab_server: { host: sftp.lab.example.org, user: hospital, private_key: "${secret:LAB_SFTP_KEY}", host_key: "SHA256:…" } }`.

### `sftp.<name>.host`

**Type:** text · **Required**

The SFTP server's host name or address. Required. Example: `host: sftp.lab.example.org`.

### `sftp.<name>.port`

**Type:** number

The server's SSH port. Default: `22`. Example: `port: 2222`.

### `sftp.<name>.user`

**Type:** text · **Required**

The account to log in as, given by the server's owner. Required. Example: `user: hospital`.

### `sftp.<name>.password`

**Type:** text

The account's password, from a secret (`${secret:LAB_SFTP_PASSWORD}`). Give this or `private_key`; a key is better where the server takes one (nothing reusable crosses the network). Example: `password: "${secret:LAB_SFTP_PASSWORD}"`.

### `sftp.<name>.private_key`

**Type:** text

The private key to log in with, as OpenSSH key text from a secret (`${secret:LAB_SFTP_KEY}`); the server's owner adds its public half to the account. `ssh-keygen -t ed25519` makes a pair. Example: `private_key: "${secret:LAB_SFTP_KEY}"`.

### `sftp.<name>.passphrase`

**Type:** text

The passphrase of an encrypted `private_key`, from a secret. Default: none (an unencrypted key, protected by the secrets file). Example: `passphrase: "${secret:LAB_SFTP_PASSPHRASE}"`.

### `sftp.<name>.host_key`

**Type:** text

The server's host key fingerprint, as `SHA256:…` (what `ssh-keygen -lf` prints for the key file), so a server pretending to be this one is refused before any password or key is sent to it.

Leave it out the first time: connecting then fails, and the error gives the fingerprint the server presented, to check with the server's administrator and paste in. When the server's key changes (a rebuilt server, another host behind the name), connections fail until it's updated: on purpose, since that's what an impostor looks like too. `any` turns the check off, for a test server on your own machine only. No default. Example: `host_key: "SHA256:nThbg6kXUpJWGl7E1IGOCspRomTxdCARLviKw6E5SY8"`.

### `sftp.<name>.application`

**Type:** text

The application behind the server, for `ductus impact`. Default: none. Example: `application: Quest Lab Results`.

### `sftp.<name>.vendor`

**Type:** text

The server's vendor (often the partner whose server it is), for `ductus impact`: "what does Quest's outage touch?". Default: none. Example: `vendor: Quest`.

### `sftp.<name>.owner`

**Type:** text

Who to ask about this server, best as an email address. Default: none. Example: `owner: lab-it@hospital.org`.

### `sftp.<name>.custom`

**Type:** names and values of your own

Your company's documentation fields for this entry (`applies_to: [resource]` in `docs-schema.yaml`). Default: none. Example: `custom: { contract: QUEST-2025 }`.

### `apis`

**Type:** names, each with settings, below

HTTP APIs the code calls with `api("<name>")` and `http` destinations send through (`api: <name>`): a base URL, headers such as the token, a timeout and TLS, set once per environment. Default: none. Example: `apis: { empi: { url: "https://empi.hospital.local/api/v2", headers: { Authorization: "Bearer ${secret:EMPI_TOKEN}" } } }`.

### `apis.<name>.url`

**Type:** text · **Required**

The API's base URL, `https://` (or `http://` on a trusted network): paths in code (`api("empi").get("/patients/1")`) and `path` on destinations go under it. Usually from the environment (`${EMPI_URL}`), since test and production differ. Required. Example: `url: "https://empi.hospital.local/api/v2"`.

### `apis.<name>.headers`

**Type:** names, each with text

Headers sent with every request, typically the credential: `Authorization: "Bearer ${secret:EMPI_TOKEN}"`, or an API key header. A header the code or a destination sets itself replaces the one here. Tokens stay out of interface code and differ per environment. Default: none. Example: `headers: { Authorization: "Bearer ${secret:EMPI_TOKEN}" }`.

### `apis.<name>.timeout`

**Type:** duration

How long one request may take, for calls and destinations that don't set their own; a call past it fails as a temporary error, so its message waits and is tried again. Default: `30s` for calls from code, `10s` for destinations. Example: `timeout: 10s`.

### `apis.<name>.ca`

**Type:** text

A CA to trust for this API's server besides the ones this server trusts (a `certificates:` entry), for an API whose certificate comes from a private CA. Default: the system's trusted CAs only. Example: `ca: hospital-ca`.

### `apis.<name>.certificate`

**Type:** text

The client certificate to present to this API (a `certificates:` entry with its key), for APIs that ask for one (mutual TLS). Default: none. Example: `certificate: ductus-client`.

### `apis.<name>.application`

**Type:** text

The application behind the API, for `ductus impact`. Default: none. Example: `application: EMPI`.

### `apis.<name>.vendor`

**Type:** text

The API's vendor, for `ductus impact`. Default: none. Example: `vendor: Rhapsody`.

### `apis.<name>.owner`

**Type:** text

Who to ask about this API, best as an email address (its certificate's expiry alerts reach them with `to: owner`). Default: none. Example: `owner: empi-team@hospital.org`.

### `apis.<name>.custom`

**Type:** names and values of your own

Your company's documentation fields for this entry (`applies_to: [resource]` in `docs-schema.yaml`). Default: none. Example: `custom: { contract: MSA-2024-17 }`.

### `ldap`

**Type:** names, each with settings, below

Directories (Active Directory, OpenLDAP) people sign in to the web UI with: `auth.ldap` in ductus.yaml names the one to use and how its groups map to roles. Default: none. Example: `ldap: { corp-ad: { url: "ldaps://dc1.hospital.org", bind_dn: "CN=svc-ductus,OU=Service,DC=hospital,DC=org", bind_password: "${secret:LDAP_BIND_PASSWORD}" } }`.

### `ldap.<name>.url`

**Type:** text · **Required**

The directory server: `ldaps://host` (TLS from the start, port 636 unless given) or `ldap://host` (port 389) with `starttls: true`. For Active Directory, a domain controller's name or the domain's (`ldaps://hospital.org` reaches any of them, through DNS). Its certificate must be for this name. Required. Example: `url: "ldaps://dc1.hospital.org"`.

### `ldap.<name>.starttls`

**Type:** `true` or `false`

With an `ldap://` URL: turn the connection into TLS (StartTLS) before anything else is sent, for directories that don't offer ldaps://. Refused if the server can't. Default: `false`. Example: `starttls: true`.

### `ldap.<name>.insecure`

**Type:** `true` or `false`

Allow plain `ldap://` with no TLS at all: people's passwords then cross the network readable to anyone in between. Only for a test directory on this machine; `ductus validate` and the engine's start warn about it. Default: `false`. Example: `insecure: true`.

### `ldap.<name>.bind_dn`

**Type:** text

The service account the engine binds as to find people and read their groups, as a DN. Give it read access to people and groups and nothing else. Without it the directory is searched anonymously, which Active Directory refuses. Default: none. Example: `bind_dn: "CN=svc-ductus,OU=Service Accounts,DC=hospital,DC=org"`.

### `ldap.<name>.bind_password`

**Type:** text

The service account's password: always from a secret (`${secret:LDAP_BIND_PASSWORD}`). When it expires, sign-ins through the directory fail (local accounts still work), so give the account a password that doesn't expire, or note when it does. Example: `bind_password: "${secret:LDAP_BIND_PASSWORD}"`.

### `ldap.<name>.ca`

**Type:** text

A CA to trust for this directory besides the ones this server trusts (a `certificates:` entry), for a domain CA this server doesn't have installed (common on Linux; Windows servers in the domain usually have it). Default: the system's trusted CAs only. Example: `ca: hospital-ca`.

### `ldap.<name>.timeout`

**Type:** duration

How long connecting, binding or one search may take before a sign-in gives up (and says the directory can't be reached). Default: `10s`. Example: `timeout: 5s`.

### `ldap.<name>.application`

**Type:** text

The application the directory is, for `ductus impact`. Default: none. Example: `application: Active Directory`.

### `ldap.<name>.vendor`

**Type:** text

The directory's vendor, for `ductus impact`. Default: none. Example: `vendor: Microsoft`.

### `ldap.<name>.owner`

**Type:** text

Who looks after the directory, best as an email address (its certificate's expiry alerts reach them with `to: owner`). Default: none. Example: `owner: directory@hospital.org`.

### `ldap.<name>.custom`

**Type:** names and values of your own

Your company's documentation fields for this entry (`applies_to: [resource]` in `docs-schema.yaml`). Default: none. Example: `custom: { ticket: CHG-1234 }`.

### `certificates`

**Type:** names, each with settings, below

Certificates the engine presents (`tls: { certificate: <name> }` on listeners and the API, `certificate: <name>` on connections it makes), and CAs it trusts or checks clients against (`ca: <name>`). Each entry is a certificate with its key (from files or secrets) or a CA alone. Their expiry is watched and alerts at 30, 14 and 7 days. Default: none. Example: `certificates: { ductus-mllp: { cert_file: /etc/ductus/tls/mllp.pem, key_file: /etc/ductus/tls/mllp.key } }`.

### `certificates.<name>.cert_file`

**Type:** text

A PEM file with the certificate to present, followed by the intermediate certificates that issued it (not the root): the "full chain" a CA sends. Goes with `key_file`.

The engine reads both files at start (a certificate it can't use stops the start, saying why) and again whenever they change, checked every minute: to renew, replace both files, with no restart. Senders that don't have the intermediates can't check a certificate without them, so leave none out. A relative path is read from the project folder; on Windows, write the drive. To keep the PEM in a secret instead, use `cert` and `key`. Example: `cert_file: /etc/ductus/tls/mllp.pem`.

### `certificates.<name>.key_file`

**Type:** text

The PEM file with the certificate's private key, unencrypted, readable by the account the engine runs as and nobody else (`chmod 600`, or the service's account only on Windows). Goes with `cert_file`, renewed with it. `ductus cert request` makes one with its CSR. Example: `key_file: /etc/ductus/tls/mllp.key`.

### `certificates.<name>.cert`

**Type:** text

The certificate (then its intermediates) as PEM text, usually from a secret (`${secret:MLLP_CERT}`), for servers where certificates live in a vault rather than in files. Goes with `key`. Read at start only: a renewal needs a restart, unlike files. Example: `cert: "${secret:MLLP_CERT}"`.

### `certificates.<name>.key`

**Type:** text

The private key as PEM text, from a secret (`${secret:MLLP_KEY}`). Goes with `cert`. Example: `key: "${secret:MLLP_KEY}"`.

### `certificates.<name>.ca`

**Type:** text

A CA's certificate (or several) as PEM text, often from a secret. An entry with only this is a CA: what `ca: <name>` trusts for a server the engine connects to, and what `clients: { ca: <name> }` checks senders' certificates against. Example: `ca: "${secret:HOSPITAL_CA}"`.

### `certificates.<name>.ca_file`

**Type:** text

A CA's certificate (or several) in a PEM file; the same as `ca`, read from a file (relative to the project folder unless written whole). Example: `ca_file: /etc/ductus/tls/hospital-ca.pem`.

### `certificates.<name>.application`

**Type:** text

The application the certificate belongs to, for `ductus impact`. Default: none. Example: `application: Ductus MLLP listener`.

### `certificates.<name>.vendor`

**Type:** text

The vendor the certificate belongs to (the CA's, for a CA entry), for `ductus impact`. Default: none. Example: `vendor: DigiCert`.

### `certificates.<name>.owner`

**Type:** text

Who renews this certificate, best as an email address: alert rules with `to: owner` mail them as it nears expiry, and the web UI shows them on its row. Default: the owner of the one interface that uses it, if just one does. Example: `owner: infrastructure@hospital.org`.

### `certificates.<name>.custom`

**Type:** names and values of your own

Your company's documentation fields for this entry (those whose `applies_to` in `docs-schema.yaml` includes `resource`). Default: none. Example: `custom: { ticket: CHG-1234 }`.

