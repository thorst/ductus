# `ductus run`

Run every interface in a project.

```text
ductus run [OPTIONS]
```

**Options**

| | What it does | Default | Environment |
|---|---|---|---|
| `--root <ROOT>` | Project folder containing `interfaces/` (folder mode; default: the current folder). |  | `DUCTUS_PROJECT` |
| `--releases <FOLDER>` | Release mode: run the release deployed to this environment (`ductus deploy`), unpacked from the database into this folder, instead of a project folder. Not with `--root` (DUCTUS_PROJECT). Example: `/var/lib/ductus/releases`. |  | `DUCTUS_RELEASES` |
| `--release-project <NAME>` | Release mode: the project this engine runs, when several are deployed to its environment on one database (default: the only one). |  | `DUCTUS_RELEASE_PROJECT` |
| `--deploy-reuse-check <DEPLOY_REUSE_CHECK>` | Release mode: how long a deploy may reuse a passing check of the same release here instead of running the gate again (only when nothing was deployed since, by the same Ductus version); `0` runs the gate at every deploy. | `1h` | `DUCTUS_DEPLOY_REUSE_CHECK` |
| `--deploy-requires <ENVIRONMENTS>` | Release mode: environments a release must have been deployed to before this one takes it (`test`, or `test,uat`), read from the record the release carries (`ductus deploy --from` brings it) and this database's deployments. `--force` doesn't skip it; a rollback (a release that ran here before) does. The record is unsigned: see the releases guide. |  | `DUCTUS_DEPLOY_REQUIRES` |
| `--deploy-branches <BRANCHES>` | Release mode: the branches a release may be made on to be deployed here (`main`, or `main,release/*`: a trailing `*` matches the start). A release made on another branch, on none (a detached HEAD without `ductus release --branch`) or without git is refused. `--force` doesn't skip it; a rollback does. The branch is a label the release carries, unsigned: the releases guide says what to rely on. |  | `DUCTUS_DEPLOY_BRANCHES` |
| `--database-url <DATABASE_URL>` | The engine's database, as a PostgreSQL URL: `postgres://ductus@db1/ductus?sslmode=verify-full`. |  | `DUCTUS_DATABASE_URL` |
| `--threads <THREADS>` | Threads in the shared worker pool (default: one per CPU core). |  | `DUCTUS_THREADS` |
| `--db-connections <DB_CONNECTIONS>` | Maximum PostgreSQL connections. | `32` | `DUCTUS_DB_CONNECTIONS` |
| `--env <ENV>` | Environment: variables from environments/<env>.yaml, secrets from secrets/<env>.json. | `dev` | `DUCTUS_ENV` |
| `--api-listen <API_LISTEN>` | Where the REST API listens (`host:port`, or `off`), instead of `api.listen` in ductus.yaml (default 127.0.0.1:7700). |  | `DUCTUS_API_LISTEN` |
| `--stop-timeout <STOP_TIMEOUT>` | On Ctrl-C or SIGTERM, how long to finish the messages in hand before exiting anyway (they stay pending for the next start). Service managers should wait longer. | `30s` | `DUCTUS_STOP_TIMEOUT` |
| `--service` | Run as a Windows service, under the service manager (the installer sets this up). |  |  |

