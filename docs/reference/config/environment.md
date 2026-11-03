# `environments/*.yaml`

`environments/<name>.yaml`: one environment's values (`dev.yaml`, `test.yaml`, `prod.yaml`). The engine reads the one it's started with (`--env prod`, or `DUCTUS_ENV`), and every `${NAME}` in interface.yaml, resources.yaml and ductus.yaml's `alerts:` takes its value from here. The same project then runs anywhere, with only this file differing.

Secrets (passwords, tokens, keys) don't go here: they're in `secrets/<name>.json`, encrypted (`ductus secret set NAME`), and written `${secret:NAME}`. `ductus validate --env <name>` checks one environment: run it for each before deploying there, and a variable missing from it is found then, not at the engine's start.

Schema for editors: `docs/schema/environment.json` (written into a project by `ductus types`). Files: `environments/*.yaml`.

### `variables`

**Type:** names and values of your own

The environment's values by name, used as `${NAME}` in the configuration. A value that stands alone keeps its type (`port: ${EPIC_PORT}` is a number when `EPIC_PORT: 6661`); inside text it's text (`url: "https://${EMPI_HOST}/api"`). Give every environment the same names. Default: none. Example: `variables: { PATIENT_DB_HOST: db1.hospital.local, EPIC_PORT: 6661 }`.

### `console`

**Type:** `debug`, `info`, `warn`, `off`

Which of interface code's `console` lines reach the engine's log in this environment: `debug` (all of them), `info` (`console.log`, `info`, `warn` and `error`), `warn` (only `console.warn` and `console.error`) or `off` (none). Code can then log freely and each environment decides, instead of `if (notProd) console.log(...)` in the code: `debug` in development and test environments, `warn` or `off` in prod. A line in code is written for every message the code runs for (a log line each, kept with the engine's own), so in prod turn it on to look into something, and back off after: `ductus validate` warns about each `console` call an environment lets through, except one that says `debug` (where logging everything is the point). `ductus test` shows every line whatever this says. Default: `info`. Example: `console: warn`.

