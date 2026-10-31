# Lookups and code tables

Interface code often needs something the message doesn't carry: a provider's NPI, a patient's
identifier in another system, the code a receiver uses for a location. This page is how to look
those up, where code tables should live, and what happens when the thing you ask is down.

Interface code has no network of its own. Everything goes through five SDK functions, so the
engine can time each call, retry what's temporary, mock it in tests and list it in
`ductus impact`:

| Function | For | Defined in |
|---|---|---|
| `db("name")` | A Postgres or SQL Server database | `databases:` in [resources.yaml](../configure/resources.md#databases) |
| `api("name")` | An HTTP API with its URL, token and TLS set once | `apis:` in [resources.yaml](../configure/resources.md#http-apis) |
| `fetch(url)` | Any other URL | the code (URL from `env()`) |
| `env("NAME")` | A value that differs per environment (a URL, a facility code) | `environments/<env>.yaml` |
| `secret("NAME")` | A password or token your code must hold itself | `ductus secret set NAME` |

**Write the name out**: `db("provider_db")`, not `db(name)`. `ductus validate` then checks the
database exists, and `ductus impact` knows this interface uses it; a name in a variable gets a
warning (`DX4`) because neither can see it ([nuance](../nuances/databases-and-apis.md#write-resource-names-out-dbpatient_db-not-dbname-level-2)).

## A database lookup

```ts
// interfaces/adt-to-billing/billing.ts, in part
// Providers' NPIs by the EHR's id, kept for an hour: they rarely change, and messages come often.
const npis = new Map<string, { npi: string; until: number }>();

async function npiOf(ehrId: string): Promise<string> {
  const known = npis.get(ehrId);
  if (known && known.until > Date.now()) return known.npi;
  const row = await db("provider_db").one<{ npi: string }>("SELECT npi FROM providers WHERE ehr_id = $1", [ehrId]);
  if (!row) throw quarantine(`provider ${ehrId} has no NPI in provider_db`);
  npis.set(ehrId, { npi: row.npi, until: Date.now() + 60 * 60 * 1000 });
  return row.npi;
}
```

- **`query(sql, params)`** gives rows as objects, **`one`** the first row or `null`,
  **`execute`** the number of rows a statement changed.
- **Always parameters, never text pasted into SQL**: `$1`, `$2`… on Postgres and SQL Server alike.
  Values from a message are whatever the sender sent.
- **SQL Server** sends text parameters as `nvarchar`: compared with an indexed `varchar` column,
  that can turn an index seek into a scan. Cast it: `WHERE ehr_id = CAST($1 AS varchar(20))`.
  One statement and one result set per call ([nuance](../nuances/databases-and-apis.md#sql-server-one-result-set-one-batch-per-call-level-3)).
- **Not found is a decision.** Here an unknown provider is quarantined, because billing can't
  bill without the NPI. For an optional field, leave it out instead. Never send a guess.
- **Many values, one query**: looking up every OBX's code one at a time is a round trip each.
  Collect them and ask once (an array parameter is `jsonb` in Postgres)
  ([nuance](../nuances/transforms.md#one-query-for-many-values-not-one-query-per-value-level-3)).
- **Writes must be safe to repeat.** A message's code can run again (a retry, a resend), so an
  `execute` that inserts uses the message's `meta.id` as a key, or an upsert.
- The login, host and TLS of the database are in resources.yaml, with the password in a secret;
  the code never sees them.

## Caching

The lookup above keeps each answer for an hour in a `Map` at the top of the file. Do that for
anything that changes rarely and is asked often (providers, locations, a code list); a feed of
100,000 messages a day then makes a few hundred queries, not 100,000.

- **A cache lives in one isolate**: each worker thread has its own copy, and it's empty after a
  restart ([nuance](../nuances/shared-pool.md#module-level-caches-live-until-the-engine-restarts-level-2)).
  Fine for a cache; never use it for anything that must be counted or remembered.
- **Bound it**: keyed by something with a known number of values (providers), with a time limit.
  A cache keyed by patient grows with every patient until the engine restarts and counts toward
  the isolate's memory ([nuance](../nuances/transforms.md#short-lived-objects-are-cheap-what-you-keep-is-what-costs-level-4)).
- **Choose the time by how wrong a stale answer is**: an NPI an hour old is fine; a patient's
  current insurance probably isn't.

## An API lookup

```ts
// lib/empi.ts, in part
/** The patient's enterprise id from the EMPI, by MRN. */
export async function enterpriseId(mrn: string): Promise<string> {
  const r = await api("empi").get(`/patients/${encodeURIComponent(mrn)}`);
  if (r.status === 404) throw quarantine(`the EMPI doesn't know MRN ${mrn}`);
  return r.json().enterpriseId;
}
```

- **`api("name")`** sends to a path under the API's URL from resources.yaml, with its headers
  (the token) and TLS added by the engine: credentials never pass through your code. Prefer it
  to `fetch()` for anything with a token ([nuance](../nuances/databases-and-apis.md#api-headers-the-apis-then-yours-level-2)).
- **`fetch(url, options)`** for a URL of its own: `method`, `headers`, `body` (an object is sent as
  JSON), `query`, `timeout` (ms, default 30 s).
- **Temporary failures wait and retry by default**: a network error, a timeout, 408, 429 or a 5xx
  makes the message wait (in order) and try again later, so an API that's down for ten minutes
  costs ten minutes, not a quarantine ([nuance](../nuances/databases-and-apis.md#fetch-and-api-retry-5xx-408-and-429-by-default-level-3)).
  Every other status comes back to your code to decide: check `r.status` or `r.ok`.
- **`retry: false`** gives you every status and turns network errors into ordinary errors (which
  quarantine unless you catch them): for a call whose failure shouldn't hold the message up,
  such as an optional enrichment.
- **`r.json()` and `r.text()`** need no `await`: the body is already read.

## Variables and secrets in code

`env("FACILITY")` reads a variable from the environment's file, and `secret("NAME")` a secret;
both throw if it isn't defined (so a missing value is a quarantine with its name, not an empty
string sent on). Prefer settings to code: a URL, a host or a token used by a connection belongs
in resources.yaml or interface.yaml (`${NAME}`, `${secret:NAME}`), where `ductus validate` checks
each environment has it. Use `env()` for values only code uses (a facility code to stamp, a
cut-off date).

## Code tables: where they should live

A code table translates one system's codes into another's. There are three good places for one,
and the right one depends on who changes it and how often:

| Where | Best when | How |
|---|---|---|
| **In code**, a `const` in `lib/` | It changes with the interface, a few times a year, by the integration team; it's small (up to a few thousand rows) | `lib/codes.ts`, reviewed in Git, tested with the interface, deployed with it |
| **In a database** the business maintains | Another team changes it often (locations, chargemaster codes), or it's large | `db("ref_db")` with a cache, as above |
| **Behind an API** (a terminology server) | The organization has one | `api("terminology")` with a cache |

```ts
// lib/codes.ts, in part
/**
 * The code's translation in `table`, or quarantine the message: an unknown code is for a person
 * to look at (add it to the table, or fix it at the source), never something to guess.
 *
 *   msg.PV1[2] = translate(PATIENT_CLASS, msg.PV1[2], "patient class");
 */
export function translate(table: Readonly<Record<string, string>>, code: string, what: string): string {
  const value = table[code];
  if (value === undefined) throw quarantine(`unknown ${what} "${code}"`);
  return value;
}
```

Whichever you choose:

- **An unknown code quarantines.** It's the first sign a sender added a code, and a person fixes
  it in minutes: add the row, then resend the quarantined messages. Passing the code on unchanged,
  or a default, puts wrong data in the receiver without anyone knowing.
- **A table in code is immutable**: `Readonly<…>` and `const`, in a shared file every interface
  imports ([shared code](index.md#code-shared-between-interfaces)).
- **A table in a database** needs the cache's time to match how fast a change must show: an
  hour, or a restart, after someone edits it. Say so to the team that maintains it.
- **Test the table**: one test per interesting code, and one for an unknown one
  ([testing](testing.md)).

There's no code-table screen or upload in Ductus today; a shared reference-data cache across
interfaces is planned (v1.4). Until then the table in Git, or a database table the business
already maintains, is the way.

## Lookups in respond mode

An HTTP source in `respond` mode answers the caller from your code, so lookups there are on the
caller's clock. Calls that don't depend on each other should run together:
`const [a, b] = await Promise.all([db("x").one(…), api("y").get(…)])`, not one after the other
([nuance](../nuances/monitoring.md#in-respond-mode-calls-run-one-after-another-unless-you-await-them-together-level-2)).
