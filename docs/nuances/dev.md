# Nuances: local development

What could surprise you here, why it is so, and what to do. Each entry has a level, 1 (day one) to 5 (expert): the [index](index.md) lists them all by level.

## `ductus dev` is a development tool, not a way to run in production (level 1)
- **What:** `ductus dev` starts a Postgres of its own for the project (data in `.ductus/postgres`, one engine at a time per project) and runs the engine against it. It ignores `DUCTUS_DATABASE_URL`, uses default Postgres settings, and nobody backs the data up. Postgres is downloaded once per user (about 30 MB).
- **Why:** to try an interface without asking anyone for a database. Production needs the DBA's Postgres (backups, monitoring, failover), and a message the engine has acknowledged must survive what the DBA protects against.
- **What to do:** use `ductus dev` on your own machine; everywhere else set `DUCTUS_DATABASE_URL` and use `ductus run`. Without a download possible, unpack the Postgres binaries yourself and set `DUCTUS_POSTGRES_DIR`. `ductus dev --reset` throws the dev data away.

## Your editor's help is the installed version's: run `ductus types` after an upgrade (level 1)
- **What:** completion, descriptions on hover and underlined mistakes in interface.yaml, resources.yaml and ductus.yaml come from the JSON Schemas `ductus types` writes into `.ductus/schema/` (with the SDK's typings in `.ductus/types/`), and those come from the `ductus` that wrote them. After an upgrade, a new setting is underlined as unknown and its description is missing until they're written again; on a laptop with an older `ductus` than the servers, the editor describes the older one. `ductus test` and `ductus validate` write them too.
- **Why:** the schemas are made from the engine's own types, so they say exactly what that version accepts, offline (an air-gapped site can't fetch them from a website), with no second copy to drift.
- **What to do:** run `ductus types` (or `test`) after installing a new version; keep developers' `ductus` at the version the servers run. `.ductus/` isn't committed, so each machine writes its own.
