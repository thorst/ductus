# Nuances: load testing an interface (`ductus bench interface`)

What could surprise you here, why it is so, and what to do. Each entry has a level, 1 (day one) to 5 (expert): the [index](index.md) lists them all by level.

## Lookups are mocks unless `--real`, and `--real` never runs against production by name (level 2)
- **What:** by default every `db()`, `api()` and `fetch()` call is answered by a mock after a set latency (5 ms for a database, 40 ms for an API), with no rows unless `--rows` gives some, and writes answered as done: nothing is connected or written. `--real` uses the environment's own databases and APIs, reads and writes, at the rate the code runs; it's refused for an environment named `prod`, `production`, `prd` or `live` (any case, and `prod-east`, `live_2`…), and asks first everywhere else.
- **Why:** a load test sends far more than a feed does, as fast as the code goes: against production's systems it would be a test's load, and its writes, on the systems patients' data lives in. The refusal goes by name because the environment's name is all the bench knows about it.
- **What to do:** use mocks for the code's numbers, with the latency the real lookup takes. Run `--real` against a test environment when that latency is what you need to know, warn its owners first, and mind its writes (an interface that inserts rows inserts thousands). **A production environment named otherwise (`main`, `site-a`) isn't caught**: name it as one of the above, or never pass its name with `--real`.

## A lookup answering nothing takes the code's other path (level 2)
- **What:** a mocked database answers no rows and a mocked API `200 {}` unless `--rows NAME=FILE` gives them something. Code that quarantines, filters or answers 404 when a lookup finds nothing then does that for every message, and the run measures that path: the guides' `adt-to-billing` quarantined 100% at 166 a second until given a provider's row. The report counts each destination's decisions and says when the lookups answering nothing is why.
- **Why:** the bench can't know what your systems hold, and the test files' `mock.db(...)` answers are inside each test's code, where the bench can't read them.
- **What to do:** give every resource the code reads a typical answer (`--rows provider_db=rows.json`, an array of objects for a database, a body for an API) and check the decisions line says `100% sent` (or the share you expect).

## A cache in your code meets the same few fixtures again and again (level 3)
- **What:** the bench cycles the interface's fixtures (two, say) for seconds, so a module-level cache keyed by something in the message (a provider, a code) is filled in the warm-up and hit on every message after: `adt-to-billing` with rows made no lookup at all, 31,000 a second, where a lookup on every message gave 166.
- **Why:** the cache works; but a real feed brings many keys, and each one's first message waits on the lookup.
- **What to do:** read the two runs as the ends of a range: a lookup on every message (no rows, or a cache emptied) and none. For a figure in between, run it on a day's real file (`--file`), whose keys repeat as production's do.

## The threads are what the config gives (level 3)
- **What:** by default the bench runs the code on as many threads as the engine would give the interface: one in the shared pool, one in respond mode, one without an ordering key (messages go one at a time), and `workers` when `isolation: dedicated`, up to its lanes (`partitions`). `--threads N` runs on N, each with its own JavaScript engine, whatever the config says.
- **Why:** the number that matters is what the interface does as configured; a run on more threads than it would get would say it keeps up when it doesn't.
- **What to do:** when the verdict says a peak needs more threads, it names the config that gives them (`isolation: dedicated`, `workers: N`, and an ordering key or more `partitions` when they're what limits it); `--threads N` checks the figure before you change it. Threads beyond the machine's cores wait for each other, which the lookups' waits show.

## Respond mode: the bench asks one request at a time (level 3)
- **What:** a respond-mode interface's requests are run one after another, so its requests a second (25 for `patient-lookup` with a 40 ms EMPI) is one caller's pace. The engine answers requests side by side while they wait on lookups, so with many callers at once one engine answers at the code's own pace (about 23,000 a second for the same code), and the verdict says both.
- **Why:** each request's own time (what a caller waits) and the engine's capacity are different questions; one request at a time measures the first exactly, and the second comes from the code's own time.
- **What to do:** for what callers wait, read `each request` (its p95, p99). For capacity, read "with enough callers at once it answers about N/s" and, above it, the engines behind a load balancer the verdict names. A real load generator against a running engine (callers on many connections) is the check when the number matters.

## A thread that waits on every lookup runs its code slower (level 4)
- **What:** with a 40 ms lookup on every message, reading the same JSON took 30–40 µs, against 4 µs with the lookups answering at once (measured in WSL, 2026-11-27): while a thread waits, its CPU core slows down and its caches fill with other work, and the first microseconds after each wake-up pay for it. So the bench measures the code's own pace in a second short run (1 s, after its warm-up) with the mocks answering at once, and the verdict's "without the waits" and respond mode's capacity use it ("44 µs of its own a request on a thread kept busy; 450 µs here").
- **Why:** both numbers are real: a worker that waits on every message runs each one slowly, which is what its CPU per message says; threads or requests side by side keep a thread busy, and then it runs at the faster pace.
- **What to do:** size CPU for a queued interface from the CPU per message (that's what one worker waiting on its lookups costs); size a respond-mode interface's capacity, or what caching a lookup would buy, from the pace kept busy. With `--real` the second run can't be made (a real lookup can't answer at once): the verdict says its figure is the slower one, and a run with mocks gives the other.
