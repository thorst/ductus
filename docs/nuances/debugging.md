# Nuances: debugging

What could surprise you here, why it is so, and what to do. Each entry has a level, 1 (day one) to 5 (expert): the [index](index.md) lists them all by level.

## `ductus test --inspect` waits for a debugger before anything runs (level 1)
- **What:** with `--inspect`, `ductus test` says `waiting for a debugger on 127.0.0.1:9229` and does nothing else until a debugger attaches and tells it to go on (VS Code and Chrome DevTools do once they've set their breakpoints). Then the test files load, so a breakpoint on a line at the top of a file (outside any function) stops too.
- **Why:** tests finish in milliseconds; without the wait they'd be over before a debugger could attach.
- **What to do:** start it from VS Code's "Ductus: debug these tests", which attaches by itself. From a terminal, attach with Chrome DevTools (chrome://inspect) or your editor; Ctrl+C ends it.

## One interface's tests per debugging run (level 1)
- **What:** `ductus test --inspect` runs the tests of one interface: the one `--interface` names (its name, or a file or folder inside its folder, which is how VS Code's configuration passes the open file), or the only interface that has tests. With several and no `--interface` it refuses and lists them.
- **Why:** each interface's tests run in a JavaScript runtime of their own, and a debugger attaches to one.
- **What to do:** open a file of the interface you're debugging (its code or a test) before choosing "Ductus: debug these tests"; add `--filter` to run one test.

## The time limit is off while a debugger is attached (level 2)
- **What:** an interface's code is stopped when it runs longer than its `timeout` without pausing ([runaway scripts](runaway.md)). While a debugger is attached, that limit is off for the code it's attached to: a call paused at a breakpoint for a minute goes on when you continue, and an endless loop runs on (the debugger's pause button stops it where it is). The heap limit stays. **The moment the debugger detaches** (you stop debugging, close VS Code or the DevTools window, or the connection drops), the limit applies again from then: a loop still running is stopped within the timeout, as any runaway, and the run ends.
- **Why:** a paused call would otherwise be stopped as a runaway as soon as it went on, and stepping through a loop would be impossible.
- **What to do:** nothing; to find an endless loop, attach, pause, and look at where it is.

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

## Breakpoints are set on your `.ts` lines; the engine runs JavaScript (level 3)
- **What:** the engine runs your TypeScript as JavaScript it makes when the file loads (types removed), with a source map inside it. Debuggers read the map, so breakpoints, the current line and stack traces are on your `.ts` lines as written. A client that ignores source maps shows the JavaScript, without types; line numbers can differ there.
- **What to do:** in VS Code nothing (`sourceMaps` is on by default); in other clients turn source maps on if they're off.
