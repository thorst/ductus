# Environments, variables and secrets

One project runs in every environment (a developer's laptop, test, production) with only its
**environment's values** differing: the hosts and ports it connects to, the folders it uses, and
its passwords and tokens. Interfaces never say which environment they're in.

| | Where | Written | Holds |
|---|---|---|---|
| **Variables** | `environments/<env>.yaml` | `${NAME}` | Anything that differs and isn't secret: hosts, ports, URLs, folders. |
| **Secrets** | `secrets/<env>.json`, encrypted | `${secret:NAME}` | Passwords, tokens, keys, webhook URLs. |

The engine uses one environment, the one it's started with: `DUCTUS_ENV=prod` (the packages'
settings file sets it), or `--env prod` on the command line. Without either it's `dev`.

## Variables

Each environment's file holds its [`variables`](../reference/config/environment.md#variables),
by name:

```yaml
# environments/prod.yaml
variables:
  EPIC_PORT: 6661
  LAB_HOST: lab.hospital.org
  LAB_PORT: 7001
  PATIENT_DB_HOST: patients-db.hospital.org
  BILLING_DB_HOST: billing-sql.hospital.org
  EMPI_URL: https://empi.hospital.org/api/v2
  QUEST_SFTP_HOST: sftp.quest.example.com
  OUTBOUND_DIR: /data/outbound
  TLS_DIR: /etc/ductus/tls
```

```yaml
# environments/dev.yaml
variables:
  EPIC_PORT: 6661
  LAB_HOST: 127.0.0.1
  LAB_PORT: 7001
  PATIENT_DB_HOST: localhost
  BILLING_DB_HOST: localhost
  EMPI_URL: http://127.0.0.1:8090/api/v2
  QUEST_SFTP_HOST: 127.0.0.1
  OUTBOUND_DIR: out
  TLS_DIR: tls
```

Then any value in interface.yaml, resources.yaml, or the `alerts:` section of ductus.yaml can be
a variable:

```yaml
# interfaces/orders-to-lab/interface.yaml
name: orders-to-lab
source: { mllp: { port: ${EPIC_PORT} } }
destination: { mllp: { host: ${LAB_HOST}, port: ${LAB_PORT} } }
```

- **A value that is only a reference keeps the variable's type**: `port: ${LAB_PORT}` is the
  number 7001. Inside text it's text: `url: "${EMPI_URL}/patients"`.
- **Give every environment the same names.** `ductus validate` checks the environment you name,
  and a variable missing from it stops the start, naming it: run `ductus validate --env prod`
  before deploying to production.
- **`$${` writes a literal `${`**, for the rare value that needs one.
- **Not in the rest of ductus.yaml**: only its `alerts:` section is read per environment (mail
  servers and webhooks differ; retention and sign-in rules shouldn't).
- **Not in code**: an interface's TypeScript reads the environment's values with `env("NAME")`
  and its secrets with `secret("NAME")`, from the same files.

**When not to use a variable:** for what is the same everywhere (an interface's port, when every
environment uses the same one, is clearer written out), and never for secrets: environment
files are plain text in Git.

## Secrets

Secrets are encrypted, one value at a time, in `secrets/<env>.json`, and **committed to Git**
encrypted: the repository has them, readable only with the environment's key, which never is.
That way a secret changes with a commit and a review like everything else, and a cloned project
has all it needs except the key.

**Make a key** for each environment, once:

```sh
ductus secret init          # prints a new random key
```

Keep it where your other production secrets live (a vault, a password manager), and give it to
the engine as `DUCTUS_SECRET_KEY`: in `/etc/ductus/ductus.env` on Linux,
`C:\ProgramData\Ductus\ductus.env` on Windows. Use a different key for each environment, so a
developer with the test key can't read production's secrets.

**Set a secret** (the value is read from the terminal, so it stays out of your shell's history):

```sh
export DUCTUS_SECRET_KEY=…              # the prod key
ductus secret set PATIENT_DB_PASSWORD --env prod
ductus secret list --env prod           # names only, never values
```

and use it anywhere a variable can go:

```yaml
# resources.yaml
databases:
  patient_db:
    postgres:
      host: ${PATIENT_DB_HOST}
      database: patients
      user: ductus
      password: ${secret:PATIENT_DB_PASSWORD}
      tls: verify-full
```

- **What belongs in a secret:** database and SFTP passwords, SFTP private keys, API tokens
  (`Authorization: "Bearer ${secret:EMPI_TOKEN}"`), the directory's service password, the mail
  server's password, chat webhook URLs (a webhook's URL is its own password), the heartbeat's
  URL, certificates and keys kept in a vault rather than in files.
- **Where values show:** `ductus secret list` gives names only, and the engine shows a webhook's
  URL by its scheme and host only. Anyone who can run `ductus` on the server with the key can
  read the values, so the key is what to protect.
- **Without the key**, a project that uses a secret can't be read for that environment: the
  engine's start and `ductus validate` stop and say which secret is missing.
- **Rotating a secret:** `ductus secret set` the new value, commit, deploy (`systemctl reload
  ductus` on Linux). Rotating the *key* means setting every secret again with the new key.

## Choosing environment names

`dev` (the default), `test` and `prod` cover most teams; any name works (`staging`,
`dr-site`). Each name needs its `environments/<name>.yaml` (the engine refuses an environment
without one, once the folder exists) and, if it uses secrets, its `secrets/<name>.json` and key.
A disaster-recovery site that connects to different hosts is its own environment.
