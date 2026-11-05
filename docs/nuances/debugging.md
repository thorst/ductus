# Nuances: debugging

What could surprise you here, why it is so, and what to do. Each entry has a level, 1 (day one) to 5 (expert): the [index](index.md) lists them all by level.

## `ductus test --inspect` waits for a debugger before anything runs (level 1)
- **What:** with `--inspect` (`ductus try --inspect` too; `ductus dev --inspect` doesn't wait, the engine runs as usual and you attach when you like), `ductus test` says `waiting for a debugger on 127.0.0.1:9229` and does nothing else until a debugger attaches and tells it to go on (VS Code and Chrome DevTools do once they've set their breakpoints). Then the test files load, so a breakpoint on a line at the top of a file (outside any function) stops too.
- **Why:** tests finish in milliseconds; without the wait they'd be over before a debugger could attach.
- **What to do:** start it from VS Code's "Ductus: debug these tests", which attaches by itself. From a terminal, attach with Chrome DevTools (chrome://inspect) or your editor; Ctrl+C ends it.

## One interface's tests per debugging run (level 1)
- **What:** `ductus test --inspect` runs the tests of one interface: the one `--interface` names (its name, or a file or folder inside its folder, which is how VS Code's configuration passes the open file), or the only interface that has tests. With several and no `--interface` it refuses and lists them.
- **Why:** each interface's tests run in a JavaScript runtime of their own, and a debugger attaches to one.
- **What to do:** open a file of the interface you're debugging (its code or a test) before choosing "Ductus: debug these tests"; add `--filter` to run one test.

## The time limit is off while a debugger is attached (level 2)
- **What:** an interface's code is stopped when it runs longer than its `timeout` without pausing ([runaway scripts](runaway.md)). While a debugger is attached, that limit is off for the code it's attached to: a call paused at a breakpoint for a minute goes on when you continue, and an endless loop runs on (the debugger's pause button stops it where it is). The heap limit stays. **The moment the debugger detaches** (you stop debugging, close VS Code or the DevTools window, or the connection drops), the limit applies again from then: a loop still running is stopped within the timeout, as any runaway: a test run ends; under `ductus dev --inspect` the message is quarantined and the interface goes on.
- **Why:** a paused call would otherwise be stopped as a runaway as soon as it went on, and stepping through a loop would be impossible.
- **What to do:** nothing; to find an endless loop, attach, pause, and look at where it is.

## Under `ductus dev --inspect`, only the debugged interface waits (level 2)
- **What:** `ductus dev --inspect adt-in` gives `adt-in` a thread and JavaScript runtime of its own for the run. While its code is paused at a breakpoint, stepping, or looping with a debugger attached, `adt-in`'s senders still get their ACKs (messages are stored and ACKed before code runs) and its messages queue up in order; every other interface receives and sends as usual. When you continue, the queue drains. An endless loop runs on while the debugger is attached (no time limit then); the moment it detaches, the loop is stopped within the interface's `timeout`, that message is quarantined ("transform stopped: the script ran longer than its … timeout"), and the messages behind it are delivered. A dedicated interface's workers share its one debugged thread for the run; a `respond` interface's HTTP callers wait for their answers while it's paused.
- **Why:** a pause stops the thread the code runs on. In the shared pool that thread runs other interfaces too, so the debugged one gets its own: what you hold up is only what you're looking at.
- **What to do:** watch the queue in the web UI if you pause for long (its messages are waiting, as they would behind a slow receiver). To find a loop, pause it: the debugger shows the line.

## Stopping `ductus dev` while paused leaves that message for the next start (level 2)
- **What:** stopping `dev` (VS Code's stop button, Ctrl-C) while the debugged interface is paused at a breakpoint ends the debugging session itself: the paused code goes on and `dev` stops at once, without waiting for anyone to press "continue". The message it was working on finished after the stop began, so it isn't sent: it stays waiting and is sent at the next start, as any message in hand when an engine stops.
- **Why:** a stop must never depend on a person at a debugger; and a stopping engine starts no new sends.
- **What to do:** nothing; the message isn't lost. Expect it to go through (and stop at your breakpoint again) at the next `ductus dev --inspect`.

## On Windows, stopping the debug session leaves dev's Postgres until the next start (level 1)
- **What:** stopping "Ductus: debug this interface" in VS Code on Windows ends `ductus dev` at once rather than asking it to stop, so the Postgres it started keeps running (a few processes named `postgres.exe`) until the next `ductus dev` in that project, which stops it and starts cleanly. Messages aren't lost: one in hand when it ended is sent at the next start. On Linux and macOS the stop asks `dev` to stop, as Ctrl-C does, and it stops its Postgres itself.
- **Why:** Windows can't ask a program running in the background to stop politely (`taskkill` without `/F` is refused for it); asked that way, VS Code would end the session and leave `dev` running, holding its ports.
- **What to do:** nothing usually; to stop the Postgres sooner, start `ductus dev` once more and Ctrl-C it, or stop `ductus dev` in its terminal (Ctrl-C) when you started it there.

## With several `--inspect` interfaces, VS Code attaches to the first (level 3)
- **What:** `ductus dev --inspect a --inspect b` offers both on the one debugger port, listed by name. Chrome DevTools (chrome://inspect) shows each and can open both; VS Code's configurations attach to the first by name only.
- **Why:** VS Code's `attachSimplePort` takes the first target a port lists.
- **What to do:** in VS Code, debug one interface at a time (open its file, "Ductus: debug this interface"); for two at once, use Chrome DevTools.

## A runaway test ends that interface's test run (level 2)
- **What:** when a test is stopped for running past the time limit (an endless loop, with or without a debugger), the interface's whole test run ends: `ductus test` reports it as `✗ failed to load tests` with "stopped: the script ran longer than its 5s timeout", the tests before it aren't reported, and those after it don't run. Other interfaces' tests run as usual.
- **Why:** stopping JavaScript that runs away also drops what the test runner was waiting on.
- **What to do:** run the interface's tests with `--filter` to find the one that loops, or attach a debugger and pause it.

## The debugger listens on this machine only, and never in `ductus run` (level 3)
- **What:** `--inspect` takes a port or an address on this machine (127.0.0.1, ::1, `localhost`); anything else is refused, saying why. The engine (`ductus run`, the services) has no debugger at all.
- **Why:** whoever reaches a debugger's port runs any code they like as the user running it, and reads every value: patient data included.
- **What to do:** to debug on another machine, forward the port: `ssh -L 9229:127.0.0.1:9229 dev-server`, then attach locally. With VS Code Remote (SSH, WSL, a container) the debugger runs on that machine with `ductus`, so "Ductus: debug these tests" works as it is. Debug production problems with the message, not on the production engine.

## A `debugger;` statement pauses every debugging session (level 1)
- **What:** `debugger;` in code or a test pauses there whenever a debugger is attached, whatever breakpoints are set; without one (the engine, `ductus test` without `--inspect`) it does nothing. `ductus validate` warns about each (`DX5`).
- **What to do:** use the editor's breakpoints; remove `debugger;` lines once you're done.

## `ductus try` sends nothing; lookups are made, writes aren't (level 2)
- **What:** `ductus try` runs each destination's function on the message and stops there: nothing is connected, sent, stored or routed. Lookups are made in the environment named (`--env`, default dev): `db().query` (on a read-only session for Postgres) and `fetch()` GET or HEAD, so the code sees that environment's data **as it is today**; such a destination is marked "it looked something up: today's data". Writes aren't made: `db().execute` and other `fetch()` methods fail inside the code with "not made in ...", and the destination is reported "not decided: writes to …: not made by `ductus try`", even when the code caught the error and went on.
- **Why:** trying a message must never change anything; and a lookup's answer from today can differ from the one the engine got then, which explains many "it decided differently" surprises.
- **What to do:** when a stored message decides differently now and the destination looked something up, check the data before the code. For code that writes, test the write with `mock.db` in `ductus test`.

## Without the environment's secrets, `ductus try` runs without databases and APIs (level 1)
- **What:** when resources.yaml can't be loaded for the environment (a `${secret:…}` not set, as on a laptop without the environment's key), `ductus try` still runs the code, without databases and APIs, and says so first: "(without databases and APIs: resources.yaml …)". A destination whose code looks something up then fails as the engine would with the resource missing.
- **Why:** most interfaces' code uses no resource; refusing to try them for a password they don't need would get in the way.
- **What to do:** for an interface that looks things up, give the environment's key (`DUCTUS_SECRET_KEY`) or try in an environment you have the secrets for.

## A message read with `--from` is in that engine's audit log (level 2)
- **What:** `ductus try --message ID --from https://ductus-prod:7700` reads the stored message through that engine's API with the token in DUCTUS_API_TOKEN. Reading a payload needs the token's `payloads` permission, and the engine records the read in its audit log (`message.payload.read`, the token's name, the message), as for the web UI. A token without it is refused: "the token may not read payloads". The code then runs on your machine, in your environment (`--env`).
- **Why:** this is how a production problem is debugged without a debugger on production: the message comes to the code. Patient data leaving the engine is what the audit log is for.
- **What to do:** use a personal token, so the audit log names you. The message is on your machine afterwards only in what `ductus try` printed: treat that output as patient data.

## Breakpoints are set on your `.ts` lines; the engine runs JavaScript (level 3)
- **What:** the engine runs your TypeScript as JavaScript it makes when the file loads (types removed), with a source map inside it. Debuggers read the map, so breakpoints, the current line and stack traces are on your `.ts` lines as written. A client that ignores source maps shows the JavaScript, without types; line numbers can differ there.
- **What to do:** in VS Code nothing (`sourceMaps` is on by default); in other clients turn source maps on if they're off.
