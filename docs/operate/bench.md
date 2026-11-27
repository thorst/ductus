# Measuring this machine: ductus bench

`ductus bench` measures what this machine and its database do with the real engine: how many
messages a second senders get ACKed, strict and relaxed, on one connection and on many; how fast
one message goes to 1, 5 and 25 destinations; a file's rows; respond-mode HTTP calls; a realistic
transform per CPU thread; and the memory of 100 and 1,000 idle interfaces. Each number comes with
what it means for sizing, in the same output. It's one command in the binary you already have,
with no scripts, no `psql` and nothing sent anywhere but this machine and its database.

**Why:** "how big a server?" and "is the new database server faster?" get answered with this
machine's own numbers instead of a vendor's. Run it on a new server before going live, after a
database move or a Postgres setting change (before and after, then [compare](#comparing-runs-ductus-bench-compare)),
and when choosing between instance types.

**When not:** it measures the engine and the database with synthetic messages delivered to
files, not your interfaces: their code, their lookups and their receivers add their own time
([nuance](../nuances/bench.md#the-benchs-messages-are-synthetic-and-its-destinations-are-files-level-2)).
For your own code, `ductus test` times each message through it. And it's not a monitoring tool:
what production does is in [monitoring](monitoring.md).

## Before you run it

- **Run it on the engine's server, against the database the engine uses, with the URL it uses**
  (`DUCTUS_DATABASE_URL`, or `--database-url`; `sslmode` included). The numbers are this machine
  to this database, not either alone
  ([nuance](../nuances/bench.md#the-numbers-are-this-machine-to-this-database-not-either-alone-level-3)).
- **It works in a schema of its own** (`ductus_bench_…`), made and dropped by it, also on Ctrl-C:
  the engine's own tables are never read or touched, so the production database is safe to bench.
  The login needs `CREATE` on the database
  ([nuance](../nuances/bench.md#the-bench-works-in-a-schema-of-its-own-and-shares-the-disk-with-whatever-else-runs-level-2)).
- **An engine running on the same database** makes it stop and say so: both share the disk and
  both would be slower meanwhile. Bench before the engine starts, or in a quiet hour with `--yes`.
- **It takes about two minutes**: three passes of every scenario, alternated, each number the
  range the passes gave. `--quick` does one pass in about 40 s, to see that it works, not for a
  number you'll decide on
  ([nuance](../nuances/bench.md#one-pass-gives-a-number-not-a-range-and-two-quick-runs-differ-by-up-to-a-quarter-level-3)).
  The timed scenarios run 2 s each by default (`--seconds`).

## A run

On a developer's workstation (WSL, Postgres on the same machine), as it printed:

```text
$ ductus bench
benching in the schema ductus_bench_1795736486_37f1a5 (dropped at the end, also on Ctrl-C)
timing 100 durable commits…
pass 1/3  MLLP strict, 1 connection: 2 s…
…
pass 3/3  memory: an engine with 100, then 1,000 idle interfaces…
ductus bench 0.7.0: DESKTOP-GCT6E6V and its database, 3 passes, alternated, each number the range they gave; timed scenarios 2 s after a second's warm-up

Machine    8 cores (Intel(R) Xeon(R) W-2225 CPU @ 4.10GHz), 15.6 GB, Ubuntu 26.04.1 LTS (Linux 6.18.58.1-microsoft-standard-WSL2)
Database   Postgres 18.6 on this machine (localhost), round trip 0.20 ms, synchronous_commit on
           in a schema of its own, ductus_bench_1795736486_37f1a5 (dropped at the end)

Commits    1.90 ms median, 13.82 ms p99 (100 durable commits, each waiting for the disk)

MLLP ingest (synthetic ADT, each message delivered to a file)
                              stored/s       ACK p50       ACK p99  delivered/s
  strict, 1 connection         608–650  1.40–1.47 ms  2.48–3.21 ms      609–650
  strict, 16 connections   5,245–6,360  2.49–2.88 ms  4.28–6.49 ms  1,148–1,573
  relaxed, 16 connections  6,380–6,662  2.18–2.23 ms  6.36–8.42 ms  1,298–1,399

Fan-out (strict, 16 connections, each message to every destination's folder)
                      stored/s  all delivered   deliveries/s
  1 destination    5,714–5,995    0.95–1.37 s    1,465–2,107
  5 destinations   4,287–4,564    0.96–1.03 s   9,667–10,453
  25 destinations  1,823–2,360    3.45–3.88 s  12,890–14,494

File ingest (one CSV, `split: records`, a row a message)
                       rows/s
  100,000 rows  52,654–56,194

HTTP respond mode (a GET answered by a transform; nothing stored)
                     requests/s           p50           p99
  1 connection      4,160–4,252  0.21–0.23 ms  0.49–0.57 ms
  16 connections  34,528–44,021  0.33–0.39 ms  0.99–1.59 ms

Transforms (the guides' ORU to FHIR transform in V8, no database)
                transforms/s   per thread/s
  1 thread     25,167–29,264  25,167–29,264
  8 threads  118,520–129,052  14,815–16,131

Memory (an engine of its own, idle, MLLP interfaces with a transform each)
                     100 idle  1,000 idle     each         per GB  1,000 ready in
  idle interfaces  110–113 MB  149–150 MB  0.04 MB  23,072–25,187     0.48–0.94 s

What it means
- A durable commit took 1.90 ms (median): a strict interface's sender waits for one per message, so one connection was answered about 526 times a second here. For scale, a million messages a day averages 12 a second.
- 16 connections stored 5,669 messages a second, 9.2× one connection's 616: Postgres flushes several connections' commits together, so senders on connections of their own add up.
- `durability: relaxed` stored 6,600 a second on 16 connections, 1.2× strict, about the same: 16 strict connections already share their flushes, so the disk isn't what limits them here and relaxed buys little on this machine. A crash of the database's server can lose a relaxed interface's last moments' messages (an engine crash never does).
- Delivery fell behind receiving over MLLP, strict, 16 connections (1,323 delivered a second of 5,669 received) and relaxed, 16 connections (1,323 delivered a second of 6,600 received): one worker delivers each destination of a shared interface, so a backlog grew. `isolation: dedicated` with `workers: N` shares its lanes (from `ordering.key`) among N; in production the destination's own speed decides it too.
- With 25 destinations 2,046 messages a second were stored and 14,347 deliveries a second made: ingest fell to 36% of one destination's 5,717: each message's ACK waits for its delivery rows too (one per destination). A feed to many receivers can be split with routes (`interface:` destinations) when its ACKs must stay fast.
- A CSV's rows were stored at 55,934 a second: a file of a million rows takes about 17.9 s to read in, then its rows go out as their destinations take them.
- Respond mode answered 42,026 requests a second on 16 kept-alive connections (0.34 ms p50; 0.22 ms with one caller): what the engine adds to a call. Your code's own lookups add theirs, and a caller opening a new connection per call pays the connect too.
- One thread ran the guides' ORU to FHIR transform 28,094 times a second, 124,952 on 8 threads (4.4× one): an interface at a peak of 150 messages a second keeps 0.5% of one thread busy with code like it, so code is rarely the limit; lookups (`db()`, `fetch`) usually are, and run while other messages are transformed.
- An idle interface with its code took about 0.04 MB of memory, so a GB holds about 25,000 of them beside the engine's own (~112 MB with 100). Traffic adds its own: messages in hand, connections, the code's data.

Measured in 110 s with synthetic messages delivered to files: your interfaces' code and destinations add their own time (`ductus test` times a message through the code).
```

A workstation's numbers, not a limit of the engine: a server's disk, CPUs and network give its
own. For the scale of real workloads: a hospital's ADT feed of a million messages a day averages
12 a second and peaks at perhaps ten times that; a nightly batch file of 200,000 rows; an API
answering a portal's lookups at a few hundred calls a second.

## Reading it, and what to do

| The number | Means | Act when |
|---|---|---|
| **Commits** (median) | The database's disk flush: each strict ACK waits for one. One connection tops out near 1 / commit time | Over 10 ms (the advice says so): the database needs a faster disk (SSD/NVMe, a write cache with a battery). A commit over 2 ms with a sender on one connection near its peak: give it more connections, or `durability: relaxed` if losing the last moments on a database crash is acceptable ([nuance](../nuances/ordering.md#durability-relaxed-can-lose-the-last-fraction-of-a-second-level-4)) |
| **Round trip** | The network to the database: added to every strict ACK | Over 1 ms (the advice says so): put the engine on the database's network segment |
| **MLLP stored/s** | What senders get ACKed, on 1 and on 16 connections | A feed's peak above the 1-connection number: ask the sender for more connections, or split the feed |
| **MLLP delivered/s** | One worker's pace for one destination of a shared interface, not a limit | A destination that must keep up with a sustained rate above it: [`isolation: dedicated`](../reference/config/interface.md#isolation) with [`workers`](../reference/config/interface.md#workers) ([nuance](../nuances/bench.md#mllps-delivered-a-second-is-one-workers-pace-not-a-limit-level-3)) |
| **Fan-out stored/s** | Ingest with every destination's delivery row written before the ACK | Ingest with your destination count below the feed's peak: split the feed with routes ([nuance](../nuances/destinations.md#many-destinations-on-one-interface-when-to-split-level-3)) |
| **File rows/s** | A CSV's rows stored, a row a message | A file that must be in within a window: its rows ÷ this is how long reading it in takes |
| **Respond requests/s, p50** | What the engine adds to a respond-mode call | Rarely: your code's lookups are the time; callers should keep connections alive |
| **Transforms per thread** | A realistic transform's cost, without lookups | Rarely: a peak near it means the code does far more than the guides' transform |
| **Memory each, per GB** | What an idle interface costs | Size RAM as this × your interfaces + the engine's own + what's in hand at peak ([nuance](../nuances/bench.md#memory-is-measured-idle-level-3)) |

`--json` gives the same report as JSON (the machine, the database, each scenario's numbers with
their passes, and the advice), to keep beside the server's records or to compare.

## Comparing runs: ductus bench compare

Save each run with `--json` and put them side by side: two machines, two instance types, a
database near and far, before and after a Postgres setting. Here the same database, plain and
over TLS (`sslmode=disable`, then `sslmode=require` added to a URL without options of its
own), in part:

```text
$ ductus bench --json --database-url "$DUCTUS_DATABASE_URL?sslmode=disable" > plain.json
$ ductus bench --json --database-url "$DUCTUS_DATABASE_URL?sslmode=require" > tls.json
$ ductus bench compare plain.json tls.json
ductus bench compare: 2 runs

                                        plain                 tls
ductus                                  0.7.0               0.7.0
host                          DESKTOP-GCT6E6V     DESKTOP-GCT6E6V
Machine                      8 cores, 15.6 GB    8 cores, 15.6 GB
OS                         Ubuntu 26.04.1 LTS  Ubuntu 26.04.1 LTS
Postgres                   18.6, this machine  18.6, this machine
round trip                            0.08 ms             0.11 ms
synchronous_commit                         on                  on
passes                                3 × 2 s             3 × 2 s
commit p50                            1.36 ms             0.84 ms
commit p99                            3.71 ms             4.03 ms

MLLP ingest (synthetic ADT, each message delivered to a file)
  strict, 1 connection
    stored/s                              654                 600  ×0.92
    ACK p50                           1.42 ms             1.55 ms  ×1.09
    ACK p99                           2.76 ms             3.00 ms  ≈
    delivered/s                           655                 600  ×0.92
  strict, 16 connections
    stored/s                            6,337               6,456  ≈
    ACK p50                           2.32 ms             2.47 ms  ×1.06
    ACK p99                           5.92 ms             4.52 ms  ≈
    delivered/s                         1,524               1,548  ≈
  relaxed, 16 connections
    stored/s                            7,546               6,950  ×0.92
    ACK p50                           1.90 ms             2.11 ms  ×1.11
    ACK p99                           6.16 ms             5.71 ms  ≈
    delivered/s                         1,374               1,449  ×1.05

Fan-out (strict, 16 connections, each message to every destination's folder)
  1 destination
    stored/s                            6,258               5,731  ×0.92
    all delivered                      1.27 s              1.35 s  ≈
    deliveries/s                        1,580               1,485  ≈
  5 destinations
    stored/s                            4,847               4,137  ≈
    all delivered                      0.97 s              1.01 s  ≈
    deliveries/s                       10,358               9,925  ≈
  25 destinations
    stored/s                            2,338               2,264  ≈
    all delivered                      3.28 s              3.11 s  ≈
    deliveries/s                       15,228              16,093  ≈
…
Each number is the median of its run's passes; the last column is tls's over plain's where their ranges don't overlap, ≈ where they do (the difference is within the spread of one run's own passes). `-`: not measured in that run (another ductus version).
```

Read it as: TLS to this database costs about 8% of the messages a sender on one connection
gets ACKed (and of relaxed ingest), and nothing that shows on 16 strict connections, where the
commit flush is the limit either way. Not a reason to go without TLS; a reason to know the
number. The rest of the report (files, respond mode, transforms, memory) doesn't touch the
database's connection and came out `≈`.

Each number is its run's median. With two runs, the last column is the second's over the
first's where their ranges don't overlap, and `≈` where they do: the difference is within the
spread of one run's own passes, so it's noise until shown otherwise. A run made with `--quick` has
no range, so every difference shows as a ratio: compare runs of three passes
([nuance](../nuances/bench.md#one-pass-gives-a-number-not-a-range-and-two-quick-runs-differ-by-up-to-a-quarter-level-3)).
A scenario only one run has shows `-` in the other's column (another version of `ductus`).

## The database's settings: ductus doctor

The bench says how fast; [`ductus doctor`](cli.md#check-every-connection-ductus-doctor) says
whether the database's settings are a risk. Its engine-database check reads them (`pg_settings`)
and says what it found on a `settings` line, then a code for each one worth knowing:

```text
$ ductus doctor
ductus doctor · environment dev · 2026-11-26 18:50:22 -05:00
on DESKTOP-GCT6E6V · ductus 0.7.0
checked 4 connection(s) in 0.2 s: 3 ok, 1 warning(s), 0 failed

✓ engine database  localhost:5432 Postgres
    used by  the engine (DUCTUS_DATABASE_URL)
    …
    commits  1.2 ms median of 20 (each waits for the disk): strict ingest ≈ 860 msg/s per sender connection, one commit per ACK; more connections share commits. A 1M/day ADT feed averages ~12 msg/s.
    settings synchronous_commit on, fsync on, wal_sync_method fdatasync, shared_buffers 128 MB, effective_cache_size 4 GB, max_connections 100 (7 open now), random_page_cost 4, effective_io_concurrency 16, autovacuum on
  → DOC004 (tuning): shared_buffers is 128 MB, Postgres's default or less: on a database server of its own, about a quarter of its RAM is usual (a restart applies it).
  → DOC005 (tuning): effective_cache_size is 4 GB, Postgres's default: on a database server of its own, half to three quarters of its RAM is usual (it's the planner's guess and allocates nothing).
  → DOC008 (tuning): random_page_cost is 4, priced for hard disks: on SSD or NVMe, 1.1 is usual, so the planner uses indexes as readily as they deserve.
…
```

A stock Postgres on a workstation, as here: three notes and a ✓.

A **warning** is a risk to messages or to a start: `fsync`, `synchronous_commit` or `autovacuum`
off, too few connections for two engines (a handover runs the old and the new one at once:
[nuance](../nuances/doctor.md#the-connections-check-counts-two-engines-for-a-handover-level-3)).
Act on each before going live. A **note**,
marked `(tuning)`, is a setting left at Postgres's default where a database server of its own is
usually tuned; the check still passes
([nuance](../nuances/doctor.md#postgress-own-defaults-are-notes-only-risks-are-warnings-level-3)).
Doctor can't see the database server's RAM or disks, so it says what a setting is usually set to,
not a number: change it with the DBA, then bench before and after. Every code, what it means and
what to do: [doctor findings](../reference/doctor.md).
