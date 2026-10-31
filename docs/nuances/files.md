# Nuances: files, SFTP and schedules

What could surprise you here, why it is so, and what to do. Each entry has a level, 1 (day one) to 5 (expert): the [index](index.md) lists them all by level.

## A file is picked up one poll after it stops changing (level 3)
- **What:** a file is ready when its size and modification time haven't changed since the previous poll (or its `marker` file exists). Expect a delay of up to two poll intervals. Dot-files are skipped.

## One file at a time, oldest first; a failure stops the rest (level 3)
- **What:** files keep their order: if one fails, later files wait. Unsplit files over 256 MB are left in place with an error.

## Cron uses the server's local time; missed runs collapse to one (level 3)
- **What:** after downtime, a schedule runs once, not once per missed time. A run is skipped if the previous run's message is still pending.

## On Linux the service can write only `/var/lib/ductus` until you add a folder (level 2)
- **What:** the package's systemd unit makes the whole file system read-only to the engine except `/var/lib/ductus` (`ProtectSystem=strict`, `ReadWritePaths=/var/lib/ductus`), and hides home folders (`ProtectHome=yes`). A file destination writing to `/data/outbound`, or a file source moving what it read to an archive outside that folder, fails with "Read-only file system" (the message waits and is tried again, as for any temporary failure). The same interface works under `ductus run` in a terminal, which has no sandbox.
- **Why:** one interface with a wrong path shouldn't be able to write anywhere on the server.
- **What to do:** add the folders interfaces use: `sudo systemctl edit ductus`, then `[Service]` and `ReadWritePaths=/data/outbound /data/inbound`, then `sudo systemctl restart ductus` (a unit change needs a restart, not a reload). Give the `ductus` user the folders' permissions too (`chown` or a group). Windows has the same idea with its own means: see "The Windows service can only touch what you give it".
