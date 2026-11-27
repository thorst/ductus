# Nuances: measuring this machine (`ductus bench`)

What could surprise you here, why it is so, and what to do. Each entry has a level, 1 (day one) to 5 (expert): the [index](index.md) lists them all by level.

## The bench works in a schema of its own, and shares the disk with whatever else runs (level 2)
- **What:** `ductus bench` makes a schema named `ductus_bench_<time>_<random>` in the database it's given, runs the engine's migrations into it and drops it at the end, also on Ctrl-C or a panic. It never reads or writes the engine's own tables, so it can run against the production database without touching a message. But the disk, the CPUs and the database's connections are shared: an engine running on the same database (the bench sees its lock connection and refuses without `--yes`) gets slower while the bench runs, and the bench's numbers come out lower than the machine can do.
- **Why:** the bench measures the real engine against the real database, which is the point, and that can't be done without the database's server doing the work.
- **What to do:** bench a new server before its engine starts, or a running one in a quiet hour with `--yes`. The login needs `CREATE` on the database (the engine's own login has it if it made its tables). A schema left by a bench that was killed (`kill -9`, a power cut) is dropped by the next bench on that database once it's a day old, and the bench says so; to drop one sooner: `DROP SCHEMA ductus_bench_… CASCADE`.

## The numbers are this machine to this database, not either alone (level 3)
- **What:** every number is the engine on the machine the bench runs on, talking to the database at the URL it's given: its disk's flush, the network's round trip, both machines' CPUs. A strict MLLP sender on 1 connection measured 608–650 messages a second with Postgres on the same machine (0.2 ms away, 1–2 ms a commit); a database one network hop away adds that round trip to every ACK, and one with a slower disk adds its flush.
- **Why:** that's what a sender sees: the ACK waits for the commit, wherever the database is.
- **What to do:** run the bench on the engine's server, against the database it will use, with the URL it will use (`sslmode` included: TLS to Postgres costs ~8% of 16-connection ingest, measured in 0.7.9). To compare two database servers or two instance types, save each run (`--json`) and put them side by side with `ductus bench compare`.

## MLLP's "delivered a second" is one worker's pace, not a limit (level 3)
- **What:** in the MLLP scenarios, `delivered/s` (~1,300–1,800 on this project's machine) is much lower than `stored/s` on 16 connections (~5,200–6,700): the bench's interface is shared, so one worker delivers its destination, one message after another. The bench's advice says when delivery fell behind receiving.
- **Why:** a shared interface has one worker per destination, which keeps order simple and costs nothing when idle. A real feed rarely arrives at 6,000 a second for long, and a real receiver is usually slower than the bench's file.
- **What to do:** a destination that must keep up with a sustained high rate gets [`isolation: dedicated`](../reference/config/interface.md#isolation) with `workers: N`, which shares its lanes (from `ordering.key`) among N workers. Watch the destination's backlog in production rather than sizing from this number ([monitoring](../operate/monitoring.md)).

## One pass gives a number, not a range, and two quick runs differ by up to a quarter (level 3)
- **What:** by default every scenario runs three times, alternated with the others, and each number is the range the passes gave. `--quick` runs each once: about a third of the time (~40 s against ~110 s on this project's machine), but two quick runs back to back differed by 5–25% on the same machine, with nothing changed.
- **Why:** a few seconds of traffic meets the disk's and the operating system's own moments (a flush, a checkpoint, another program); passes spread across the run show how much that moves the numbers.
- **What to do:** use `--quick` to see that everything works, the default three passes for any number you'll decide on. `ductus bench compare` shows `≈` where two runs' ranges overlap (the difference is within the runs' own spread) and a ratio only where they don't; for a one-pass run it can only show a ratio, so treat a quick run's ratio under ~1.3× as noise.

## The bench's messages are synthetic and its destinations are files (level 2)
- **What:** the scenarios send generated HL7 ADT messages (and one CSV, and one HTTP GET), to interfaces whose destinations write files on this machine and whose code is small. Your interfaces' code, lookups (`db()`, `api()`) and receivers add their own time, and a receiver is often the slowest part.
- **Why:** the bench measures what the engine and the database can do, which is the same for every project; what your interfaces add is theirs.
- **What to do:** read the bench as the floor the engine gives you. For your own code, `ductus test` times each message through it; the transforms scenario shows what a realistic transform (the guides' ORU to FHIR) costs a thread.

## Memory is measured idle (level 3)
- **What:** the memory scenario starts an engine of its own with 100, then 1,000 MLLP interfaces with a transform each, and reads its memory 2 seconds after it has started, before any message: 150 MB at 1,000 on this project's machine (~0.04 MB an interface beside the engine's own ~110 MB).
- **Why:** idle memory is what an interface costs just for existing, which decides how many fit; what traffic adds depends on the traffic.
- **What to do:** size memory as the bench's idle number plus what's in hand at your peak: messages being transformed, open connections (~15 kB each, ~23 kB with TLS), and what your code keeps (module-level caches, code tables).
