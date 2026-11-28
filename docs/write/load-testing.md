# Load testing an interface: ductus bench interface

`ductus bench interface <name>` runs one interface's real code on its fixtures, in a loop, for a
few seconds, and says what it does on this machine: messages a second, each message's time,
where that time goes (reading the message, your code, each lookup, writing the result), what
each destination decided, the CPU and memory it takes, and **what limits it**, in a sentence,
with what your peak would need. Its databases and APIs are mocked at a set latency, so nothing
is connected, stored or sent, and no database is needed: it runs on a laptop beside
`ductus test`.

**Why:** "will this interface keep up?" gets answered before it meets the feed, by the code
itself and not a guess: a transform that does 30,000 messages a second isn't the problem; a
lookup taking 5 ms on every message caps one worker near 170 a second, and the bench says which
of the two you have, and what config gets past it.

**When:** after writing or changing an interface that does lookups, has a peak you know, or
answers callers (respond mode); before giving it `workers`; when a destination falls behind in
production and you want to know whether the code or a lookup is why.

**When not:** it doesn't measure the engine, the database's disk or the receivers: those are
[`ductus bench`](../operate/bench.md)'s (the same machine's ceiling for storing and delivering).
And it doesn't test correctness: that's [`ductus test`](testing.md). Every option is in the
[reference](../reference/cli/bench.md#ductus-bench-interface).

## A run

`adt-to-billing` in the guides' project maps an admission to a billing encounter and looks up
the attending's NPI in `provider_db`, keeping each for an hour ([the code](lookups.md)). With nothing but its name:

```text
$ ductus bench interface adt-to-billing
adt-to-billing: 5.0 s on 1 thread (the shared pool: one worker per interface), 2 messages from its fixtures, in turn; databases and APIs mocked, nothing sent
  166 msg/s, 832 messages
  each message: p50 6.02 ms, p95 6.29 ms, p99 6.65 ms, slowest 8.32 ms
  CPU: 882 µs a message on its thread; JavaScript heap: 2.09 MB a thread after the warm-up
  billing: 100% quarantined; decode 38 µs, code 287 µs, lookups 5.33 ms, serialize 0.0 µs
      first: a01.hl7: provider 1234 has no NPI in provider_db
  database `provider_db`: 1.00 calls per message, each mocked at 5.00 ms, answering no rows (give some: `--rows provider_db=rows.json`)
  → some messages were quarantined or retried with lookups answering nothing: the numbers are for that path. Give the lookups rows (`--rows`) to measure the usual one.
  → The lookups are the limit: a message waits 5.33 ms of its 6.02 ms on them, most on database `provider_db`; without the waits this code would do about 4,900 msg/s a thread (202 µs of its own a message on a thread kept busy; 684 µs here, the thread waking after each wait).
  → At 150/s (a busy feed's peak; give yours with `--peak`) it uses 90% of what it does here (166/s), and 13% of one core.
```

Line by line:

- **The first line** says what ran: 5 s (after a second's warm-up, not counted) on **one thread,
  because that's what the engine gives it**: a shared interface has one worker
  ([nuance](../nuances/load-testing.md#the-threads-are-what-the-config-gives-level-3)). Its two
  fixtures were run in turn, again and again.
- **msg/s and each message's p50/p95/p99** are every destination's function run on one message,
  one after another on that thread.
- **CPU** is the thread's own CPU time per message: what it costs the machine. The **heap** is
  what the code keeps after a collection (the SDK, your modules and what they hold).
- **Each destination's line** says what it decided (sent, filtered, quarantined, retried) and
  where its time went: `decode` (reading the message as its format says), `code` (your function,
  less its waits), `lookups` (its waits on the mocks), `serialize` (writing the result).
- **Each lookup** is counted: calls per message, the latency it was given, and what it answered.
- **The → lines** are the verdict: what limits it, and what the peak means.

Here every message was **quarantined**: the mock answered no rows, so the code took its "no NPI"
path, which is not the path a real admission takes, and the bench says so
([nuance](../nuances/load-testing.md#a-lookup-answering-nothing-takes-the-codes-other-path-level-2)).
Give it rows.

## Giving lookups an answer: --rows

`--rows NAME=FILE` answers that resource from a JSON file: a database's rows (an array of
objects), or an API's body.

```text
$ echo '[{ "npi": "1497758544" }]' > providers.json
$ ductus bench interface adt-to-billing --rows provider_db=providers.json
adt-to-billing: 5.0 s on 1 thread (the shared pool: one worker per interface), 2 messages from its fixtures, in turn; databases and APIs mocked, nothing sent
  31,398 msg/s, 157,020 messages
  each message: p50 0.02 ms, p95 0.07 ms, p99 0.12 ms, slowest 2.86 ms
  CPU: 32 µs a message on its thread; JavaScript heap: 2.11 MB a thread after the warm-up
  billing: 100% sent; decode 2.2 µs, code 22 µs, serialize 2.1 µs
  no database or API calls
  → The code is the limit: 31,398 msg/s a thread; of each message's 32 µs, the most goes to the functions' own code (22 µs).
  → At 150/s (a busy feed's peak; give yours with `--peak`) it uses 0.5% of what it does here (31,398/s), and 0.5% of one core.
```

Now every message is sent, and **no lookup was made**: the code keeps each NPI for an hour, and
the fixtures have one attending, so after the warm-up every message found it in the cache. That
is the cache doing its job, and also the bench's few fixtures flattering it: a real feed has
hundreds of attendings, and each one's first message waits on the database
([nuance](../nuances/load-testing.md#a-cache-in-your-code-meets-the-same-few-fixtures-again-and-again-level-3)).
The two runs are the two ends: 166 a second with a lookup on every message, 31,000 with none.
Your feed is between them, nearer the second the more the cache holds.

## Lookup latency: --mock

A mock answers after its latency, without using the CPU, as a real lookup's wait doesn't: by
default **5 ms for a database** (an indexed query on the same network) and **40 ms for an API**
(`api()` or `fetch()`). Set them by kind or by resources.yaml name, which wins over its kind:
`--mock db=2ms --mock provider_db=12ms` gives every database 2 ms and `provider_db` 12. The
report says each lookup's latency, so a run is never read without it.

Use what the real system takes: its owner's number, production's
(`ductus_resource_call_seconds` in [metrics](../reference/metrics.md)), or one `--real` run
below. Writes (`db().execute`, a
POST) are answered by the mock as done, so an interface that writes can be load-tested too;
nothing is written.

## Threads and your peak: --peak, --threads

`--peak N` is the interface's busiest rate in messages a second (the default is 150, a busy
feed's peak, said as such). The verdict says what share of what it does here that is, or, when
it's more, the threads and config it would take:

```text
$ ductus bench interface results-to-fhir --peak 3000
results-to-fhir: 5.0 s on 1 thread (the shared pool: one worker per interface), 1 message from its fixtures, again and again; databases and APIs mocked, nothing sent
  24,449 msg/s, 122,301 messages
  each message: p50 0.03 ms, p95 0.11 ms, p99 0.17 ms, slowest 2.46 ms
  CPU: 41 µs a message on its thread; JavaScript heap: 2.14 MB a thread after the warm-up
  fhir: 100% sent; decode 2.0 µs, code 28 µs, serialize 6.6 µs
  no database or API calls
  → The code is the limit: 24,449 msg/s a thread; of each message's 41 µs, the most goes to the functions' own code (28 µs).
  → At your peak of 3,000/s it uses 12% of what it does here (24,449/s), and 12% of one core.
```

The guides' ORU to FHIR transform builds a Bundle of several resources per result message; one
thread does about 24,000 a second here, so a lab's results feed at 3,000 a second at its worst
keeps an eighth of a core busy. When a peak is more than one thread does, the verdict names the
config: `isolation: dedicated` with `workers: N`, and an ordering key first if the interface has
none (without one, messages go one at a time whatever the workers:
[nuance](../nuances/ordering.md#without-an-ordering-key-more-workers-dont-help-level-3)).
`--threads N` then runs it on N threads, each with its own JavaScript engine, to check:

```text
$ ductus bench interface results-to-fhir --threads 4 --peak 60000
results-to-fhir: 5.0 s on 4 threads (`--threads 4`), 1 message from its fixtures, again and again; databases and APIs mocked, nothing sent
  94,515 msg/s (23,629 per thread), 472,748 messages
  each message: p50 0.03 ms, p95 0.06 ms, p99 0.13 ms, slowest 9.73 ms
  CPU: 42 µs a message on its thread; JavaScript heap: 2.14 MB a thread after the warm-up
  fhir: 100% sent; decode 2.2 µs, code 29 µs, serialize 6.9 µs
  no database or API calls
  → The code is the limit: 23,629 msg/s a thread; of each message's 42 µs, the most goes to the functions' own code (29 µs).
  → At your peak of 60,000/s it uses 63% of what it does here (94,515/s), its code keeping 2.5 cores busy.
```

Four threads did four times one (the machine has 8 cores). Threads beyond the cores wait for
each other, and the lookups' waits show it ("the threads outnumber the cores").

**Lookups decide most interfaces' pace, not their code.** With 5 ms on every message, one thread
does under 200 a second, so a feed of 150 a second uses 90% of its one worker (the first run
above): a burst then waits, and a slower database falls behind. The verdict's "without the
waits this code would do about 4,900 msg/s" is the code's own pace, measured apart on a thread
kept busy ([nuance](../nuances/load-testing.md#a-thread-that-waits-on-every-lookup-runs-its-code-slower-level-4)):
it's what caching the lookup, or more workers waiting side by side, can buy.

## An API that answers at once: respond mode

An HTTP source in `mode: respond` answers its caller from the code
([HTTP sources](../configure/sources.md#http)). The guides' `patient-lookup` gives the patient
portal a patient's enterprise id from the EMPI:

```yaml
# interfaces/patient-lookup/interface.yaml, in part
source:
  http: { port: 8081, path: "/patients/{mrn}", method: GET, mode: respond }
script: lookup.ts
format: json
```

```ts
// interfaces/patient-lookup/lookup.ts
import { api, type HttpRequest, response } from "@ductus/sdk";

/** GET /patients/{mrn}: the EMPI's enterprise id for the MRN, or 404 when it has none. */
export async function transform(req: HttpRequest) {
  const mrn = req.params.mrn;
  const found = await api("empi").get(`/patients/${encodeURIComponent(mrn)}`);
  if (found.status === 404) return response({ status: 404, body: { error: `no patient with MRN ${mrn}` } });
  return response({ body: { mrn, enterpriseId: found.json().enterpriseId } });
}
```

Its fixtures are requests. A file is a request's body, sent as the source's method to its path;
a file named **`*.request.json`** is a whole request, each part optional: `method`, `path`,
`params` (the path's `{…}` parts), `query`, `headers` and `body` (sent as its JSON when it isn't
a string). `fixtures/known.request.json`:

```json
{ "params": { "mrn": "123456" } }
```

```text
$ echo '{ "enterpriseId": "E-77821" }' > empi.json
$ ductus bench interface patient-lookup --rows empi=empi.json --peak 400
patient-lookup: 5.0 s on 1 thread (respond mode: one thread answers its requests), 1 request from its fixtures, again and again; databases and APIs mocked, nothing sent
  25 requests/s, 123 requests
  each request: p50 40.84 ms, p95 41.03 ms, p99 41.06 ms, slowest 41.13 ms
  CPU: 660 µs a request on its thread; JavaScript heap: 2.01 MB a thread after the warm-up
  response: 100% answered 200; decode 28 µs, code 288 µs, lookups 40.36 ms, serialize 48 µs
  API `empi`: 1.00 calls per request, each mocked at 40.00 ms, with the rows given
  → The lookups are the limit: a request waits 40.36 ms of its 40.81 ms on them, most on API `empi`; without the waits this code would do about 23,000 requests/s a thread (44 µs of its own a request on a thread kept busy; 450 µs here, the thread waking after each wait). The engine answers requests side by side while they wait, so with enough callers at once it answers about 23,000/s; each caller still waits 40.81 ms.
  → At your peak of 400/s it uses 2% of what one engine answers (22,921/s), and 2% of one core.
```

Read it as two numbers: **each caller waits 41 ms**, nearly all of it the EMPI's; and **one
engine answers about 23,000 a second** when many callers come at once, because the engine
answers requests side by side while they wait. The bench sends one request at a time, so its own
25 a second is one caller's pace, not the engine's
([nuance](../nuances/load-testing.md#respond-mode-the-bench-asks-one-request-at-a-time-level-3)).
Answers are counted by HTTP status; a thrown error counts as the status the engine answers it
with (422, 503 or 500), and an answer that's an error after a lookup answered nothing is said, as
for a quarantine above. When the peak is more than one engine answers, the verdict says how many
engines behind a load balancer it would take: one thread answers a respond-mode interface.

A caller's wait is the lookups' sum when they're awaited one after another: two lookups that
don't need each other's answer, awaited together (`Promise.all`), wait as long as the slower
([nuance](../nuances/monitoring.md#in-respond-mode-calls-run-one-after-another-unless-you-await-them-together-level-2)).
The bench counts each moment once, so `lookups` shows it.

## Against a test environment's real systems: --real

When a mock's latency is a guess and the real one matters, `--real` uses the environment's own
databases and APIs (its resources.yaml, `--env`) as the engine would. **Its reads and its writes
are made**, at the rate the code runs, so the test systems get the load: it says so and asks
first, naming the environment (`--yes` for a script). **It's refused for a production
environment**, by name: `prod`, `production`, `prd`, `live` and their `prod-…` variants
([nuance](../nuances/load-testing.md#lookups-are-mocks-unless---real-and---real-never-runs-against-production-by-name-level-2)).

```text
$ ductus bench interface patient-lookup --env prod --real
Error: `prod` reads as a production environment (`prod`): a load test is never run against production's databases and APIs. Run it against a test environment (`--env test --real`), or with mocks (without `--real`)
```

Against `test`, its EMPI a stand-in on the same machine:

```text
$ ductus bench interface patient-lookup --env test --real --yes
`patient-lookup` against environment `test`'s real systems: database `provider_db`, API `billing`, API `empi`, API `fhir`. For a second's warm-up and 5 s, as fast as the code runs: its reads and its writes (db().execute, POST, PUT...) are made.
patient-lookup: 5.0 s on 1 thread (respond mode: one thread answers its requests), 1 request from its fixtures, again and again; `test`'s real databases and APIs, nothing else sent
  1,840 requests/s, 9,201 requests
  each request: p50 0.48 ms, p95 0.97 ms, p99 1.21 ms, slowest 2.54 ms
  CPU: 161 µs a request on its thread; JavaScript heap: 2.03 MB a thread after the warm-up
  response: 100% answered 200; decode 6.7 µs, code 86 µs, lookups 426 µs, serialize 7.9 µs
  API `empi`: 1.00 calls per request, 0.43 ms on average
  → The lookups are the limit: a request waits 426 µs of its 544 µs on them, most on API `empi`; without the waits this code would do about 8,500 requests/s a thread (its pace between the waits, the thread waking after each: kept busy it's faster; without `--real` the bench measures that too). The engine answers requests side by side while they wait, so with enough callers at once it answers about 8,500/s; each caller still waits 544 µs.
  → At 150/s (a busy feed's peak; give yours with `--peak`) it uses 2% of what one engine answers (8,529/s), and 2% of one core.
```

Each lookup now shows what the real one took (here 0.43 ms: a stand-in next door, not an EMPI
across the network), and failed calls are counted. Take that number back to `--mock` for runs
that don't load the test system. The code's own pace can't be measured kept busy here (a real
lookup can't be made instant), so the verdict's "without the waits" is the slower pace between
them, said as such; a run without `--real` measures it.

## What to do with it

| The report says | Means | Do |
|---|---|---|
| **The lookups are the limit**, a queued interface | Each message waits on them, one at a time per worker | Cache what changes rarely ([lookups](lookups.md)); one query for many values ([nuance](../nuances/transforms.md#one-query-for-many-values-not-one-query-per-value-level-3)); `Promise.all` for lookups that don't depend on each other; for a peak above it, the workers the verdict names |
| **The code is the limit** | Your function's own time, per thread | Rarely a problem (tens of thousands a second is usual). Above your peak: the workers the verdict names. Far below what the guides' transforms do: look at loops over many items ([nuance](../nuances/transforms.md#loops-over-thousands-of-items-give-objects-one-shape-level-4)) |
| **Quarantined or retried with lookups answering nothing** | The run measured the error path | Give the resource rows (`--rows`) |
| **At your peak it uses** over ~70% | A burst or a slower lookup and it falls behind | Treat it as full: cache, or more workers |
| **Needs about N threads** | One worker can't reach the peak | `isolation: dedicated`, `workers: N` (and an ordering key, then `partitions`, when it says so); check with `--threads N` |
| **Needs about N engines** (respond mode) | One engine's answering thread can't | N engines behind a load balancer, or a lookup cached |
| **Heap** growing between your runs as fixtures grow | The code keeps something per message | Look for a module-level array or `Map` that only grows ([nuance](../nuances/transforms.md#short-lived-objects-are-cheap-what-you-keep-is-what-costs-level-4)) |

`--json` gives the same as one document (the machine, the mocks, every number, and the verdict's
sentences), to keep beside a change or compare in CI. `--count N` runs exactly N messages instead
of `--seconds`; `--file F` runs a file instead of the fixtures, split into records as its source
would (`split: records`), to measure on a day's real file.
