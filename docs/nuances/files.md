# Nuances: files, SFTP and schedules

What could surprise you here, why it is so, and what to do. Each entry has a level, 1 (day one) to 5 (expert): the [index](index.md) lists them all by level.

## A file is picked up one poll after it stops changing (level 3)
- **What:** a file is ready when its size and modification time haven't changed since the previous poll (or its `marker` file exists). Expect a delay of up to two poll intervals. Dot-files are skipped.

## One file at a time, oldest first; a failure stops the rest (level 3)
- **What:** files keep their order: if one fails, later files wait. Unsplit files over 256 MB are left in place with an error. With `pgp:`, a file that won't decrypt or verify doesn't stop the rest: it's quarantined and the next is read (see [OpenPGP](pgp.md#a-file-that-wont-open-is-quarantined-and-the-next-file-is-read-level-2)).

## Missed runs collapse to one; a run waits for the one before (level 3)
- **What:** after downtime, a schedule runs once, not once per missed time. A run is skipped if the previous run's message is still pending.
- **Why:** a job that ran ten times in a row at start (a census, an export) would send ten copies of the same thing; a run's message is the run, so a second one while the first waits would do the work twice.
- **What to do:** write the code to cover the time since `previous` (in the run's message), not a fixed hour: then one run after downtime covers it all.

## Cron and monitor times are on the site's clock: the server's, unless you set `timezone` (level 2)
- **What:** `cron`, `monitor` hours and deadlines, and alerts' quiet hours are read on one clock: ductus.yaml's `timezone`, by default the server's own zone (on Windows, the service account's). Most containers and many cloud servers run on UTC, so there `cron: "0 6 * * *"` runs at 06:00 UTC (midnight in Chicago in winter) and `between: "06:00-22:00"` is the wrong sixteen hours. A schedule's own `timezone:` wins over the engine's for its `cron`; a run's `due` is told on its clock with the offset (`2026-11-22T06:00:00-06:00`). `every:` counts elapsed time and takes no zone.
- **Why:** nothing changes for a site that sets nothing; a site that sets it gets one answer on Windows, Linux and in a container, from the zone database built into the binary (a Ductus release brings new zone rules, not the server's updates).
- **What to do:** on a server or container that runs on UTC, set `timezone: America/Chicago` (your IANA zone) in ductus.yaml. Give a schedule its own (`timezone: America/New_York`) for a partner who works on another clock.

## Daylight saving: a skipped time runs at the next minute, a repeated time runs once (level 3)
- **What:** on the night clocks go forward, a fixed time inside the skipped hour (`30 2 * * *` where 02:00 jumps to 03:00) runs at the first minute after the gap (03:00). On the night they go back, a fixed time inside the repeated hour (`30 1 * * *`) runs once, the first time. A pattern that repeats within the hour (`*/15 * * * *`) skips nothing real: it runs through both halves of a repeated hour and has no runs in a skipped one. Monitor hours and quiet hours are wall-clock too: a night of `22:00-06:00` is 9 hours long once a year and 7 hours once a year.
- **Why:** a daily job skipped once a year is a missing file nobody notices until a partner asks; one run twice is a duplicate. Running once, as soon as the clock allows, is what people expect of "every day at 02:30".
- **What to do:** nothing for daily jobs. Avoid 01:00–03:00 for jobs that must run exactly 24 hours apart (an export covering "the last 24 hours" covers 23 or 25 on those nights): cover the time since `previous` instead, or schedule them on a UTC clock (`timezone: UTC`).

## On Linux the service can write only `/var/lib/ductus` until you add a folder (level 2)
- **What:** the package's systemd unit makes the whole file system read-only to the engine except `/var/lib/ductus` (`ProtectSystem=strict`, `ReadWritePaths=/var/lib/ductus`), and hides home folders (`ProtectHome=yes`). A file destination writing to `/data/outbound`, or a file source moving what it read to an archive outside that folder, fails with "Read-only file system" (the message waits and is tried again, as for any temporary failure). The same interface works under `ductus run` in a terminal, which has no sandbox.
- **Why:** one interface with a wrong path shouldn't be able to write anywhere on the server.
- **What to do:** add the folders interfaces use: `sudo systemctl edit ductus`, then `[Service]` and `ReadWritePaths=/data/outbound /data/inbound`, then `sudo systemctl restart ductus` (a unit change needs a restart, not a reload). Give the `ductus` user the folders' permissions too (`chown` or a group). Windows has the same idea with its own means: see "The Windows service can only touch what you give it".
