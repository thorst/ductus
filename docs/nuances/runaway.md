# Nuances: runaway scripts

What could surprise you here, why it is so, and what to do. Each entry has a level, 1 (day one) to 5 (expert): the [index](index.md) lists them all by level.

## `timeout` counts time without pausing, not total time (level 2)
- **What:** each run of your code may last `timeout` (default 5 s) without giving up the thread. Waiting on `db()` or `fetch()` doesn't count: a transform waiting 20 s on a slow database is fine; a loop running 6 s is not.
- **Why:** waiting doesn't hold up anyone; a loop holds up every interface on the thread.

## A runaway message is quarantined; three in a row stop the interface (level 2)
- **What:** code past its `timeout` is stopped and its message quarantined ("transform stopped: ..."), and the interface carries on: it might have been one bad message. Three timeouts in a row, or one heap overflow, **stop the interface**: new messages are still received and ACKed, and wait as pending. A stopped interface needs an engine restart once the code is fixed (a restart button comes with the admin API).
- **What to do:** guard loops that depend on message content (segment counts, repetition indexes) so they always end.

## Neighbours wait while runaway code runs (level 4)
- **What:** until the engine stops it, runaway code holds its thread for up to its `timeout`. Every other interface on that thread waits that long.
- **What to do:** keep `timeout` close to what the code really needs (`timeout: 500ms` is plenty for most HL7 transforms). Put interfaces with heavy or untrusted code in `isolation: dedicated`.

## The heap limit is shared by everything in an isolate (level 4)
- **What:** `isolates.heap_mb` (in `ductus.yaml`, default 512) applies to the whole isolate. In the shared pool, a heap overflow is blamed on whichever interface was running when memory ran out: usually the one allocating, but not necessarily the one that has been filling the heap slowly (e.g. an unbounded cache).
- **What to do:** bound caches; give memory-hungry interfaces their own isolate.

## A runaway after `await` interrupts every async transform on its thread (level 5)
- **What:** code that runs away *after an `await`* is stopped inside the JavaScript event loop. V8 then drops every queued continuation on that isolate, not just the runaway's. The engine fails every async transform in flight on that thread: the culprit is quarantined, the rest are **retried**. A retried transform may already have made some of its database or API calls, and an abandoned original may still finish a call that was in flight.
- **What to do:** this is one more reason to make writes idempotent (see above).

## A runaway in a shared library after `await` can't be blamed on anyone (level 5)
- **What:** to tell whose code ran away, the engine looks for an interface's files on the JavaScript call stack. Code in a shared library file, resumed after an `await`, has none, so nobody can be quarantined or stopped: every transform in flight on the thread is retried, and the problem can come back.
- **What to do:** keep loops over message content in the interface's own files, or call the library from the interface without awaiting in between.
