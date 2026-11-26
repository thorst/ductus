# `ductus bench`

Measure what this machine and its database do with the real engine, and what the numbers mean for sizing: durable commits, the round trip to the database, messages stored per second over MLLP (strict on 1 and 16 connections, relaxed on 16) with each ACK's wait and delivery's pace, fan-out to 1, 5 and 25 destinations, a CSV file's rows, HTTP respond mode, the guides' ORU transform per CPU thread, and the memory of 100 and 1,000 idle interfaces. Every scenario runs three times, alternated, and each number is the range they gave (`--quick`: once). Runs in a schema of its own in the database, made and dropped by it (the engine's own data is never read or touched), so the login needs `CREATE` on the database. Nothing is sent anywhere but this machine.

```text
ductus bench [OPTIONS]
```

**Options**

| | What it does | Default | Environment |
|---|---|---|---|
| `--seconds <SECONDS>` | Seconds each timed scenario (MLLP, HTTP, transforms) measures for, after a second's warm-up (1–600). Fan-out, file and memory run a set amount of work instead. | `2` |  |
| `--quick` | One pass instead of three: quicker, and the numbers come without their range. |  |  |
| `--json` | As JSON (the machine, the database, each scenario's numbers and the advice), to keep or compare with another machine's. |  |  |
| `--yes` | Bench although an engine is running on this database (both are slower meanwhile; its data is still never touched). |  |  |
| `--database-url <DATABASE_URL>` | The database to bench, as a PostgreSQL URL: `postgres://ductus@db1/ductus?sslmode=verify-full`. |  | `DUCTUS_DATABASE_URL` |

