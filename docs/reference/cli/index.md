# Command line

`ductus` is one program: the engine (`ductus run`), the tools around it (`validate`, `test`, `doctor`, `replay`), and the commands that talk to a running engine through its REST API (`status`, `drain`, `pause`; these need a token, `--token` or `DUCTUS_TOKEN`). Every command explains itself with `--help`; these pages say the same, with every option.

Options can also come from the environment (the variable each one names), or from a file of `NAME=value` lines given with `--env-file`, which the Linux packages and the Windows service use (`/etc/ductus/ductus.env`, `C:\ProgramData\Ductus\ductus.env`).

| Command | What it does |
|---|---|
| [`ductus init`](init.md) | Start a new project: settings, environments, resources and a first interface, ready for `ductus test`. |
| [`ductus run`](run.md) | Run every interface in a project. |
| [`ductus dev`](dev.md) | Run the project with a Postgres of its own, for local development: nothing to install. |
| [`ductus test`](test.md) | Run each interface's `*.test.ts` files. |
| [`ductus validate`](validate.md) | Check configs, code loading and TypeScript types for an environment (exits 1 on problems). |
| [`ductus upgrade`](upgrade.md) | Before an upgrade, run with the new version's binary (from the portable archive, on the server or beside it), reading only: the migrations its start would apply and how big the tables they work through are, the engines on the database and their versions, the project validated against the database and its tests run with the new SDK, deprecated settings in use. |
| [`ductus replay`](replay.md) | Run a period's messages through this version's code again, sending nothing, and compare what each destination's code decides (sent and what, filtered, quarantined) with the installed version run beside it, with what was recorded, and with kept outputs (`save_output`): the evidence a change board asks for before an upgrade or a code change. |
| [`ductus handover`](handover.md) | Hand the running engine over to the binary now on disk and the project as it is now, refusing no connection (Linux: the package's `systemctl reload ductus` runs it): an upgrade or a project deploy. |
| [`ductus new`](new.md) | Create something new in a project. |
| [`ductus ports`](ports.md) | List the ports and HTTP routes a project listens on, the hosts it connects to, and port conflicts, for an environment (exits 1 on conflicts). |
| [`ductus impact`](impact.md) | Which interfaces a vendor, application or host touches, how, and who owns them: for a vendor breach or a VPN/IP change. |
| [`ductus doctor`](doctor.md) | Check every connection the project needs, from this server: DNS, TCP (with the address a firewall request needs), TLS (certificate, expiry), then each system's own login. |
| [`ductus docs`](docs.md) | The company-defined documentation fields (`docs-schema.yaml`) and their values (`custom:`) for every interface, destination and resource. |
| [`ductus owners`](owners.md) | Who owns which interface, destination and resources.yaml entry (`about.owner`, `owner` on a destination someone else owns, `owner` in resources.yaml), and, with a database, whether each owner is an account (username, email or display name), a team in `owners:` or nobody. |
| [`ductus user`](user.md) | Manage the people who sign in (in the engine's database; that is how the first admin is made). |
| [`ductus token`](token.md) | Manage REST API tokens (in the engine's database). |
| [`ductus audit`](audit.md) | The audit log (who did what), straight from the engine's database. |
| [`ductus drain`](drain.md) | Stop taking new messages and deliver what's stored (for upgrades, server moves, config changes that need an empty queue): MLLP listeners close, queued HTTP answers 503, file pickup and schedules pause. |
| [`ductus resume`](resume.md) | Take new messages again after `ductus drain`; or, with an interface, resume its paused destination (it sends its backlog in order). |
| [`ductus pause`](pause.md) | Pause a destination at once: nothing is sent or retried until `ductus resume <interface> [destination]`; messages keep queueing. |
| [`ductus retention`](retention.md) | How long each interface keeps its messages (`retention:` in interface.yaml), changes in progress, shorter values waiting for a confirmation, and expired partitions that had to stay. |
| [`ductus hotspots`](hotspots.md) | Where each destination's backlog sits: the lanes messages wait in (the ordering key's hash), the first message of each (key, age, attempts, last error), and which worker a failing message holds up. |
| [`ductus alerts`](alerts.md) | Alerts (`alerts:` in ductus.yaml): what is open, from the running engine's REST API (viewer token); or `ductus alerts test <channel>`, which sends a test notification straight from ductus.yaml (no engine needed: check the mail server or webhook before going live). |
| [`ductus secret`](secret.md) | Manage encrypted secrets (secrets/<env>.json; key in DUCTUS_SECRET_KEY). |
| [`ductus cert`](cert.md) | Certificates for `tls:` on listeners (`certificates:` in resources.yaml). |
| [`ductus service`](service.md) | The Windows service (as an administrator). |
| [`ductus api-spec`](api-spec.md) | Print the REST API's OpenAPI document (the same as `GET /api/v1/openapi.json`, without a running engine). |
| [`ductus types`](types.md) | Write what an editor needs to help with the project, for the version installed: the SDK's typings (.ductus/types) and a tsconfig.json for the interfaces' code, and the configuration files' schemas (.ductus/schema) with VS Code's settings pointing at them, for completion, descriptions on hover and mistakes underlined in interface.yaml, resources.yaml and ductus.yaml. |
| [`ductus status`](status.md) | Show message counts by interface and status (from the database, also while the engine is down); with a token, also each interface's and destination's health from the running engine (silent, behind, stalled, paused, catching up...). |
| [`ductus gen`](gen.md) | Send synthetic ADT messages over MLLP. |
| [`ductus sink`](sink.md) | Receive messages over MLLP, ACK them, and verify per-patient ordering. |

## Options for every command

| | What it does | Default | Environment |
|---|---|---|---|
| `--env-file <FILE>` | Read settings from this file (`NAME=value` lines, the names of the DUCTUS_* variables); variables already set win. Read before anything else, in `main`. |  | `DUCTUS_ENV_FILE` |
