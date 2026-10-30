# `environments/*.yaml`

`environments/<name>.yaml`: one environment's values (`dev.yaml`, `test.yaml`, `prod.yaml`). The engine reads the one it's started with (`--env prod`, or `DUCTUS_ENV`), and every `${NAME}` in interface.yaml, resources.yaml and ductus.yaml's `alerts:` takes its value from here. The same project then runs anywhere, with only this file differing.

Secrets (passwords, tokens, keys) don't go here: they're in `secrets/<name>.json`, encrypted (`ductus secret set NAME`), and written `${secret:NAME}`. `ductus validate` checks every environment, so a variable missing from one is found before it's deployed.

Schema for editors: `docs/schema/environment.json` (written into a project by `ductus types`). Files: `environments/*.yaml`.

### `variables`

**Type:** names and values of your own

The environment's values by name, used as `${NAME}` in the configuration. A value that stands alone keeps its type (`port: ${EPIC_PORT}` is a number when `EPIC_PORT: 6661`); inside text it's text (`url: "https://${EMPI_HOST}/api"`). Give every environment the same names. Default: none. Example: `variables: { PATIENT_DB_HOST: db1.hospital.local, EPIC_PORT: 6661 }`.

