# Nuances: databases and APIs

What could surprise you here, why it is so, and what to do. Each entry has a level, 1 (day one) to 5 (expert): the [index](index.md) lists them all by level.

## SQL Server: one result set, one batch per call (level 3)
- **What:** only the first result set is returned. Each `query`/`execute` is its own batch, so a transaction must begin and end within one call. Text parameters are sent as `nvarchar`: comparing them with a `varchar` column can skip its index (cast the parameter).

## `fetch()` and `api()` retry 5xx, 408 and 429 by default (level 3)
- **What:** those statuses become temporary errors (the message waits and retries). Pass `retry: false` to handle them yourself. Bodies are text only, up to 16 MB.

## `api()` headers: the API's, then yours (level 2)
- **What:** an API in `resources.yaml` brings its URL, headers and timeout. A header your code (or an HTTP destination) sets with the same name replaces the API's. Paths go under the API's URL (`/patients/1` under `https://h/api/v2` is `https://h/api/v2/patients/1`); a full URL is refused.
- **Why:** the token lives in one place, per environment, and never passes through interface code; overriding is for the rare call that needs something else.
- **What to do:** keep credentials in the API's `headers` (with `${secret:...}`), not in code. Tests answer with `mock.api("name", ...)`; `mock.fetch` doesn't answer `api()` calls.

## Write resource names out: `db("patient_db")`, not `db(name)` (level 2)
- **What:** `ductus validate` checks that every database and API named in code exists in `resources.yaml`, and `ductus impact` uses the same names to know which interfaces use what. A name computed at run time can't be seen before the code runs: validate warns (DX4), and impact doesn't know that code uses it. The engine itself doesn't check names at startup: a wrong name quarantines messages at run time.
- **What to do:** write the name in the call. If a shared helper takes a name, have callers pass a literal through a small wrapper per resource, or accept the warning knowingly. Run `ductus validate` before deploying (in CI).

## Dedicated connection pools only for dedicated interfaces (level 4)
- **What:** `dedicated_pools` needs `isolation: dedicated`; shared-pool interfaces use the shared pools from `resources.yaml`.
