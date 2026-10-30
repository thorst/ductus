# REST API

Status, messages, drain mode and reports of a running Ductus engine. Every call but `health` needs `Authorization: Bearer <token>` (create one with `ductus token create`). Browsers sign in instead (`POST /session`) and send the session cookie; their changes also carry `X-Ductus: 1`.

The API is served by the engine at `/api/v1`, where `api.listen` in ductus.yaml says (`http://127.0.0.1:7700/api/v1` by default; HTTPS with `api.tls`). The web UI uses only this API, so anything it does, a script can do. The machine-readable description (OpenAPI 3.1) is at `/api/v1/openapi.json` on any engine, and `ductus api-spec` prints it without one.

**Who may call what.** Each operation says who may call it:

| | |
|---|---|
| public | anyone, without signing in (`/health`, for load balancers; signing in) |
| signed in | any person or token |
| a person | a person signed in with a password (their own account's settings) |
| operator | the operator or admin role: changes such as resend, pause, drain |
| admin | the admin role: accounts and tokens |
| audit | the `audit` permission (admins have it) |

A token acts with its own role, and a person's token never above that person. Reading a message's payload also needs the `payloads` permission, and is written in the audit log first.

**Errors** come back as JSON, `{"error": "what went wrong, in words"}`, with the HTTP status saying what kind: 400 a request that can't be done as asked, 401 not signed in, 403 not allowed, 404 nothing there, 409 not now (it changed meanwhile), 503 the engine can't do it now (the database is unreachable, or it couldn't write the audit row first).

| Part | What it covers |
|---|---|
| [Engine](engine.md) | The engine, its interfaces and live updates. |
| [Session](session.md) | Signing a browser in and out. |
| [Your account](me.md) | The signed-in user's own account: password, sessions, tokens, appearance. |
| [Users](users.md) | People who sign in (admin). |
| [Messages](messages.md) | The message browser, the quarantine, and resending or correcting messages. |
| [Drain](drain.md) | Stop taking messages and deliver what's stored. |
| [Reports](reports.md) | Impact, owners and connection checks. |
| [Alerts](alerts.md) | Alerts: what is wrong, acknowledging and silencing, and where notifications go. |
| [Audit](audit.md) | Who did what: the audit log (the `audit` permission) and your own activity. |
| [Retention](retention.md) | How long messages are kept, and changes to it. |
| [Tokens](tokens.md) | API tokens (admin). |
