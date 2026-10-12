# Security

Ductus is early development software for healthcare integration: **don't put real patient data through it yet.** It has had no external security audit; one is planned before 1.0 (internal review first, then free programs once the project is public, a paid firm when there is income).

## Reporting a vulnerability

Please report it privately, not in a public issue:

- GitHub: **Security > Report a vulnerability** on this repository (private vulnerability reporting), or
- email **admin@getductus.com**.

Include what you found, how to reproduce it, and the version or commit. This is a one-person project, so replies are best effort, but a report is read and taken seriously; I'll say what I plan to do and credit you if you like once it is fixed.

## What counts

Anything that lets someone read or change messages they shouldn't, run code outside the interface sandbox (interface code is meant to reach the network and disk only through `db()`, `api()`, `fetch()`, `env()`, `secret()`), get past the API's tokens and roles, learn a secret, or stop the engine from a remote, unauthenticated request. The rules the engine keeps are in [docs/architecture.md](docs/architecture.md#invariants-never-break-these); known limits (no TLS on the engine's own listeners yet, any viewer token can read payloads) are listed in the project status and are tracked, not secret.

## Supported versions

Only the latest commit on `main`. There are no releases yet.
