# The project

Everything an engine runs is in one folder of plain files, the **project**: its interfaces, their
code and tests, and a few files of settings. It lives in Git, so every change is reviewed,
recorded and can be undone; the engine runs what's in the folder, and never writes there.

**Why files and Git** rather than settings in a database behind a GUI: a change is a diff someone
reviews before it goes live, the history says who changed what and why, the same project runs
in test and production with only its environment's values differing, and rolling back is a
`git revert`. Nothing about an interface exists only inside a running engine.

## The layout

```text
hospital/
├── ductus.yaml                    the project's settings: retention, the API, sign-in, alerts
├── resources.yaml                 what interfaces share, by name: databases, servers, APIs, certificates
├── docs-schema.yaml               your company's own documentation fields (optional)
├── environments/
│   ├── dev.yaml                   each environment's values: hosts, ports, paths
│   ├── test.yaml
│   └── prod.yaml
├── secrets/
│   ├── dev.json                   each environment's secrets, encrypted (committed)
│   └── prod.json
├── interfaces/
│   ├── adt-in/
│   │   ├── interface.yaml         one interface: where messages come from and go
│   │   ├── transform.ts           its code (optional)
│   │   ├── transform.test.ts      its tests
│   │   └── fixtures/              sample messages the tests read
│   └── orders-to-lab/
│       └── …
└── lib/                           code several interfaces import (optional; any name)
```

| File | What it holds | Page |
|---|---|---|
| `interfaces/<name>/interface.yaml` | One interface: its source, format, destinations, and how messages are delivered, kept and watched. | [interface.yaml](interface.md) |
| `resources.yaml` | Databases, SFTP servers, HTTP APIs, directories and certificates, each defined once by a name that interfaces and code use. No address or credential is ever written in an interface or its code. | [resources.yaml](resources.md) |
| `ductus.yaml` | Settings for the whole project and the engine that runs it. Optional: every setting has a default. | [ductus.yaml](ductus-yaml.md) |
| `environments/<env>.yaml` | The values that differ between environments (`${NAME}` in the other files). | [Environments](environments.md) |
| `secrets/<env>.json` | Passwords, tokens and keys, encrypted (`${secret:NAME}`). | [Secrets](environments.md#secrets) |
| `docs-schema.yaml` | Documentation fields your company keeps about interfaces, beyond Ductus's own. Optional. | [Documentation fields](documentation-fields.md) |

`ductus init` makes a new project with all of these but `docs-schema.yaml` and `secrets/`, and one
interface; `ductus new interface <name> --source mllp|http|file|sftp|schedule` adds another,
with its code, a test and a sample message, on a free port.

## How the engine reads it

At start, the engine reads `ductus.yaml`, then the environment it was started with
(`DUCTUS_ENV`, or `--env`), fills every `${NAME}` and `${secret:NAME}` in from it, reads
`resources.yaml`, then every `interfaces/*/interface.yaml`, and loads each interface's code. Any
mistake stops the start with the file and what's wrong: a typo in a setting's name too, since
every file refuses keys it doesn't know. The engine doesn't read its configuration again while it
runs (only certificate files, every minute, so a renewal needs no restart); a change takes effect
at the next start, or with `systemctl reload ductus` on Linux
([deploying](../nuances.md#deploying-project-changes-systemctl-reload-level-1)).

`ductus validate` reads it the same way without starting anything, for an environment
(`--env prod`), and also type-checks every interface's code with the TypeScript compiler built
into the engine. Run it before every commit, and in CI; with `DUCTUS_DATABASE_URL` set it also
checks the project against the database, reading only. `ductus test` runs every interface's
tests.

## In your editor

`ductus types` (which `ductus test` and `ductus validate` also run) writes what VS Code needs into
the project:

- `.ductus/types/`: the SDK's typings, so `transform.ts` knows `Message`, `db()`, `api()` and
  every HL7 field's name, and `tsconfig.json` pointing at them;
- `.ductus/schema/`: a JSON Schema for each configuration file, and `.vscode/settings.json`
  mapping them to the files (unless it already maps them elsewhere).

With Red Hat's YAML extension, every setting then completes as you type, shows its description
(the same text as the [reference](../reference/config/index.md)) on hover, and is underlined when
it's wrong. They come from the `ductus` you have installed, so they match the engine that will
run the project: run `ductus types` again after an upgrade. `.ductus/` isn't committed (the
`.gitignore` from `ductus init` leaves it out).

## Values written once

Two rules keep a project readable as it grows:

- **Anything that differs between environments is a variable** (`${LAB_HOST}`), defined in each
  `environments/<env>.yaml`. The interface itself is the same file in test and production.
- **Anything an interface connects to that others might too is a resource**, by name: code says
  `db("patient_db")`, a destination says `api: billing`, a certificate is `ductus-mllp`. When the
  database moves, one line changes, and `ductus impact` can say which interfaces it touches.
