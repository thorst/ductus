# Reference

Everything Ductus takes and gives, listed: every command, API operation, setting, Event Log ID, metric and audit action. These pages are made from the engine itself (`ductus reference`), so they always match the version they come with. The guides explain how the pieces fit together; the [nuances](../nuances/index.md) say what could surprise you.

- [Command line](cli/index.md): every `ductus` command and option.
- [REST API](api/index.md): every operation under `/api/v1`, who may call it, and what it takes and returns.
- [Configuration files](config/index.md): every setting of `interface.yaml`, `resources.yaml`, `ductus.yaml`, the environment files and `docs-schema.yaml`.
- [Event Log IDs](event-log.md): what the Windows service writes to the Event Log.
- [Prometheus metrics](metrics.md): what `GET /api/v1/metrics` serves.
- [Audit actions](audit.md): what the audit log records.
- [Libraries and versions](libraries.md): what each build contains.
