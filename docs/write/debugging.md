# Debugging: breakpoints in your code

When a test fails and the reason isn't obvious from the values it prints, stop the code where it
goes wrong and look: the values of every variable, the call that led there, what an expression
gives, one line at a time. `ductus test --inspect` runs an interface's tests under a debugger,
`ductus try --inspect` [one message](#one-message-ductus-try), a file or one the engine stored,
and `ductus dev --inspect` [a running interface](#a-running-interface-ductus-dev---inspect), with
messages arriving from its senders while the rest of your dev engine carries on.

**No extension is needed.** The engine speaks the protocol every JavaScript debugger speaks (the
Chrome DevTools Protocol, as Node and Deno do), so VS Code's built-in debugger, Chrome DevTools,
JetBrains IDEs and anything else that can attach to Node attach to Ductus too.

| You use | What to do |
|---|---|
| **VS Code** | open a file of the interface (its code or a test), set breakpoints, Run and Debug → **Ductus: debug these tests**; or open a message file (a fixture) → **Ductus: debug this message**; or open its code → **Ductus: debug this interface**, a running dev engine ([below](#vs-code)) |
| **Chrome or Edge** | `ductus test --inspect --interface adt-in` (or `ductus dev --inspect adt-in`) in a terminal, then chrome://inspect → **inspect** ([below](#chrome-devtools)) |
| **JetBrains, Neovim, others** | `ductus test --inspect`, then attach to a Node.js process on 127.0.0.1:9229 ([below](#other-debuggers)) |

## VS Code

`ductus types` (also run by every `ductus test` and `ductus validate`) adds three configurations
to `.vscode/launch.json`: only added, beside your own, and never over one of the same name.

```text
$ cat .vscode/launch.json
{
  "version": "0.2.0",
  "configurations": [
    {
      "type": "node",
      "request": "launch",
      "name": "Ductus: debug these tests",
      "runtimeExecutable": "ductus",
      "runtimeArgs": [
        "test",
        "--inspect",
        "--interface",
        "${fileDirname}"
      ],
      "attachSimplePort": 9229,
      "console": "internalConsole",
      "outputCapture": "std"
    },
    {
      "type": "node",
      "request": "launch",
      "name": "Ductus: debug this message",
      "runtimeExecutable": "ductus",
      "runtimeArgs": [
        "try",
        "${file}",
        "${file}",
        "--inspect"
      ],
      "attachSimplePort": 9229,
      "console": "internalConsole",
      "outputCapture": "std"
    },
    {
      "type": "node",
      "request": "launch",
      "name": "Ductus: debug this interface",
      "runtimeExecutable": "ductus",
      "runtimeArgs": [
        "dev",
        "--inspect",
        "${fileDirname}"
      ],
      "attachSimplePort": 9229,
      "console": "internalConsole",
      "outputCapture": "std",
      "timeout": 120000,
      "killBehavior": "polite",
      "windows": {
        "killBehavior": "forceful"
      }
    }
  ]
}
```

**Ductus: debug this interface** starts [`ductus dev`](../install/dev.md) with the interface
whose file you have open under the debugger
([below](#a-running-interface-ductus-dev---inspect)). Its `timeout` (120 s) is how long VS Code
keeps trying to attach: `dev` starts its Postgres first, and the first run downloads it. Stopping
the session (the red square) asks `dev` to stop, as Ctrl-C does, so it stops its Postgres too
(`killBehavior: polite`). **On Windows** a program in the background can't be asked to stop that
way, so there the session ends `dev` at once (`windows: { killBehavior: forceful }`, which VS Code
uses on Windows) and the next `ductus dev` stops the Postgres it left
([nuance](../nuances/debugging.md#on-windows-stopping-the-debug-session-leaves-devs-postgres-until-the-next-start-level-1)).

**Ductus: debug this message** runs the file you have open as one message through its
interface ([`ductus try`](#one-message-ductus-try)): open a fixture (`interfaces/adt-in/fixtures/a01.hl7`),
set breakpoints in the interface's code, F5. The file names its interface by the folder it's in,
so `${file}` is both.

1. **Open a file of the interface** you want to debug: its code (`route.ts`) or a test
   (`route.test.ts`). `${fileDirname}` is that file's folder, and `--interface` takes a folder
   inside an interface's as that interface.
2. **Set breakpoints** in the gutter of your `.ts` files, the code or the tests.
3. **Run and Debug** (Ctrl+Shift+D), choose **Ductus: debug these tests**, and start (F5).

VS Code starts `ductus test --inspect`, attaches to it, and the tests run: at a breakpoint you see
the variables, the call stack and can evaluate expressions (Watch, or the Debug Console), step
(F10, F11) and continue (F5). The tests' results and the code's `console` lines are in the Debug
Console. When the run ends, the session does.

Here is what VS Code's debugger sees, from the [debugging demo](#checked-in-ci): CI drives VS
Code's debugger the way VS Code does, on this guide's example project, with a breakpoint on the
`msg.set("MSH-5", "LAB")` line (18) of the lab's function and `--filter` choosing one test:

```ts
// interfaces/adt-in/route.ts, in part
export function toLab(msg: Message) {
  if (!LAB_EVENTS.has(msg.MSH[9][2])) return null; // not for the lab: filtered, no reason needed
  if (msg.PID[3][1].startsWith("TEST")) return kill("test patient");

  msg.PV1[2] = translate(PATIENT_CLASS, msg.PV1[2], "patient class");
  for (const name of new Set(msg.segmentNames)) {
    if (name.startsWith("Z")) msg.remove(name); // the EHR's own segments mean nothing to the lab
  }
  msg.set("MSH-5", "LAB");
  return msg;
}
```

```text
output: adt-in: waiting for a debugger on 127.0.0.1:9229 (ws://127.0.0.1:9229/bd56edfb-61cf-3e4d-6d6b-42c579c4fa14): VS Code's "Ductus: debug these tests", or chrome://inspect in Chrome
output: debugger attached: no time limit on the tests while it is
stopped: breakpoint
at: toLab /tmp/tmp.5yF9PBXEgK/interfaces/adt-in/route.ts:18
locals: msg=Proxy(MessageImpl) {data: Array(4), index: null, delimiters: {…}}, this=undefined
msg.PV1[2] + " for a " + msg.MSH[9][2] = 'IN for a A01'
output: debugger detached: the time limit applies again
output: adt-in
output:   ✓ the lab gets visits, with its own patient class and no Z segments (192ms)
output: 1 passed, 0 failed
```

The patient class has been translated (`IN`) by the time the breakpoint is reached. The line is
line 18 of the `.ts` as written: the engine runs the code as JavaScript without its types, and
debuggers read the source map it adds, so lines are yours
([nuance](../nuances/debugging.md#breakpoints-are-set-on-your-ts-lines-the-engine-runs-javascript-level-3)).

- **On Windows**, `runtimeExecutable: "ductus"` is `ductus.exe` on the PATH (the MSI puts its
  folder there; with the zip, add it, or write the whole path in your copy of the
  configuration). If the project is in WSL, open it in VS Code through the WSL extension, so
  `ductus` is Linux's.
- **Your own copy:** to give it other options (`--filter "A31"`, another port), copy the
  configuration under another name: `ductus types` leaves it alone.

## Chrome DevTools

```sh
ductus test --inspect --interface adt-in
```

```text
adt-in: waiting for a debugger on 127.0.0.1:9229 (ws://127.0.0.1:9229/…): VS Code's "Ductus: debug these tests", or chrome://inspect in Chrome
```

In Chrome or Edge, open `chrome://inspect` (`edge://inspect`): the run is listed under **Remote
Target** as "adt-in tests" (127.0.0.1:9229 is looked at by default). Click **inspect**: DevTools
opens, the tests start, and stop at `debugger;` statements and the breakpoints you set in the
Sources panel, on the `.ts` with its types. To set breakpoints before anything runs, put a
`debugger;` line where you want to start, and remove it after
([nuance](../nuances/debugging.md#a-debugger-statement-pauses-every-debugging-session-level-1)).

## Other debuggers

Anything that attaches to a Node.js process attaches to `ductus test --inspect`: JetBrains IDEs
("Attach to Node.js/Chrome", host 127.0.0.1, port 9229), Neovim's nvim-dap with js-debug,
Zed. The endpoints are Node's: `GET /json/list` lists the target with its
`webSocketDebuggerUrl`, `GET /json/version` says `"Protocol-Version": "1.3"`.

## The command

| Option | What it does |
|---|---|
| `--inspect` | listen on 127.0.0.1:9229, wait for a debugger, then run the tests under it |
| `--inspect=127.0.0.1:9230` (or `--inspect=9230`) | another port: two runs at once, or 9229 taken by a Node debugger |
| `--interface adt-in` | the interface: its name, or a file or folder inside its folder |
| `--filter "A31"` | only the tests whose name contains the text, as without a debugger |
| `--json` | the results as JSON Lines on stdout, as without a debugger ([testing](testing.md#for-editors-and-ci-json)); what the debugger is doing goes to stderr |

- **It waits** for a debugger before loading any code, then for the debugger to say "go on"
  (VS Code and DevTools do once their breakpoints are set), so a breakpoint on a file's top-level
  code stops too ([nuance](../nuances/debugging.md#ductus-test---inspect-waits-for-a-debugger-before-anything-runs-level-1)).
  Ctrl+C ends a run nobody attached to.
- **One interface per run**: `--interface`, or the only interface with tests; with several it
  says which have tests ([nuance](../nuances/debugging.md#one-interfaces-tests-per-debugging-run-level-1)).
- **This machine only**: `--inspect=0.0.0.0:9229` is refused, because whoever reaches the port
  runs any code as you and reads every value. To debug on a server, forward the port with
  `ssh -L 9229:127.0.0.1:9229 server` ([nuance](../nuances/debugging.md#the-debugger-listens-on-this-machine-only-and-never-in-ductus-run-level-3)).

```text
$ ductus test --inspect
Error: a debugger attaches to one interface's tests at a time: add `--interface <name>` (those with tests: adt-in, other)
$ ductus test --inspect=0.0.0.0:9229
Error: `--inspect=0.0.0.0:9229`: the debugger listens on this machine only (127.0.0.1 or ::1): whoever reaches it runs any code as this user and reads every message. To debug from another machine, forward the port: `ssh -L 9229:127.0.0.1:9229 <this machine>`, or VS Code Remote's port forwarding
```

## One message: `ductus try`

`ductus try` runs one message through an interface's code, every destination, and says what each
would do with it now: sent and what, filtered and why, quarantined and why. **It sends nothing and
writes nothing.** It answers "what would this message do?" with or without a debugger: after a
change, for a message a sender complains about, for one that was quarantined.

```text
$ ductus try adt-in --fixture a01.hl7
adt-in: fixture a01.hl7, received 2026-11-05 14:57:58 UTC, through the code as it is now (code version 593b53e3a495); nothing sent
  (without databases and APIs: resources.yaml in ./resources.yaml: missing in environment `dev`: secret:BILLING_TOKEN, secret:EMPI_TOKEN, secret:FHIR_TOKEN, secret:PROVIDER_DB_PASSWORD)
  billing: sent
      MSH|^~\&|EHR|HOSP|ADT|HOSP|20261031083000-0500||ADT^A01^ADT_A01|MSG00001|P|2.5
      EVN|A01|20261031083000-0500
      PID|1||123456^^^HOSP^MR||SMITH^ANNA^M||19800214|F|||12 MAIN ST^^SPRINGFIELD^IL^62701
      PV1|1|I|4W^401^A||||1234^JONES^PAT^^^DR|||MED|||||||||V9001|||||||||||||||||||||||||20261031083000-0500
      ZPV|EHR-ONLY|42
  lab: sent
      MSH|^~\&|EHR|HOSP|LAB|HOSP|20261031083000-0500||ADT^A01^ADT_A01|MSG00001|P|2.5
      EVN|A01|20261031083000-0500
      PID|1||123456^^^HOSP^MR||SMITH^ANNA^M||19800214|F|||12 MAIN ST^^SPRINGFIELD^IL^62701
      PV1|1|IN|4W^401^A||||1234^JONES^PAT^^^DR|||MED|||||||||V9001|||||||||||||||||||||||||20261031083000-0500
```

| The message | Written |
|---|---|
| a file, as the sender sends it | `ductus try adt-in a01.hl7` |
| a fixture of the interface | `ductus try adt-in --fixture a01.hl7` |
| one the engine stored, from its database | `ductus try adt-in --message 20042988` (DUCTUS_DATABASE_URL) |
| one a running engine stored, through its API | `ductus try adt-in --message 20042988 --from https://ductus-prod:7700` (a token with `payloads` in DUCTUS_API_TOKEN) |

The interface is its name or a file or folder inside its folder, as for `ductus test`. `--env`
chooses whose values and resources the code uses (default `dev`); `--json` gives one document
(the `code_version` tried; each destination's `decision`, `output` or `reason`, `console`,
`recorded`); `--inspect` runs it under a debugger, as `ductus test --inspect` does. The first line
names the [code version](../operate/deployments.md#the-code-version) of the code it runs: the one a
start of this project would run.

**A stored message** comes with what each destination did with it then, beside what the code
decides now: here the code was changed after the message went through, so billing would drop it
today (from the [try demo](#checked-in-ci)):

```text
$ ductus try interfaces/adt --message 21198140
try-demo: message 21198140, received 2026-11-05 14:57:56 UTC, through the code as it is now (code version ae5123af1d2c); nothing sent
  billing: filtered
    then: sent
          decided by code version 07cd7ba85180, not this one (ae5123af1d2c)
  lab: sent
      MSH|^~\&|EPIC|HOSP|LAB|HOSP|20261105095756||ADT^A08|1|P|2.5
      EVN|A08|20261105095756
      PID|1||100001^^^MRN||JOHNSON^JAMES||19010101|M
      PV1|1|I|4W^401^A
    then: sent, the same output
          decided by code version 07cd7ba85180, not this one (ae5123af1d2c)
```

`then: sent, the same output` compares the output too, for a destination that keeps what it sent
(`save_output`); otherwise `then:` compares the decision. **`decided by code version …, not this
one`** says the record was decided by other code than the code tried (0.7.3.2 on: each delivery
[names the code that decided it](../operate/deployments.md#which-code-decided-each-delivery)), so a
difference from it is that change as much as anything else; nothing is said when it was the same
code, or when the record names none (decided before 0.7.3.2, or by a person).

- **Nothing leaves.** Destinations aren't connected, routes aren't stored, the receiver gets
  nothing. Lookups are made (`db().query`, a `fetch()` GET), so the code sees the environment's
  data as it is today, which may not be what it saw then: such a destination says "it looked
  something up: today's data". Writes (`db().execute`, a POST) aren't made: the destination says
  "not decided: writes to database `audit_db`: not made by `ductus try`"
  ([nuance](../nuances/debugging.md#ductus-try-sends-nothing-lookups-are-made-writes-arent-level-2)).
- **The code's clock** (`new Date()`) is when the message was received, and `meta.id` its id (0
  for a file, received now), as the engine had them.
- **Without the environment's secrets** (a laptop without the production key), the code runs
  without databases and APIs and says so first, as above; an interface that uses none is tried in
  full ([nuance](../nuances/debugging.md#without-the-environments-secrets-ductus-try-runs-without-databases-and-apis-level-1)).
- **Production messages come to the code, not the debugger to production.** `--from` reads the
  message through the engine's API, which needs the `payloads` permission and records the read in
  its audit log as every payload read is; the code runs on your machine
  ([nuance](../nuances/debugging.md#a-message-read-with---from-is-in-that-engines-audit-log-level-2)).
  A token without it is refused: "the token may not read payloads: give it the `payloads`
  permission". The message is then on your machine: handle it as the patient data it is.
- **Not for interfaces that answer HTTP requests themselves** (`respond`): test their function
  with `ductus test`.

## A running interface: `ductus dev --inspect`

Some problems only show with the real thing: the sender's messages as they come, in their order,
with the lookups and the other interfaces around them. `ductus dev --inspect adt-in` runs your
[dev engine](../install/dev.md) as usual, with `adt-in`'s code under a debugger: a breakpoint
stops the message being worked on, and you look at it as in a test.

```sh
ductus dev --inspect adt-in
```

**Only that interface waits.** It runs on a thread and JavaScript runtime of its own for the run,
so while it's paused (or stepping, or looping under the debugger):

- **its senders still get their ACKs**: the engine stores and ACKs a message before any code
  runs, so they see nothing, and its messages queue up in order behind the one you're looking at;
- **every other interface goes on**, receiving and sending as usual;
- **when you continue, the queue drains** in order.

Measured (`scripts/bench-inspect.sh`, [below](#what-it-costs)): another interface's throughput
and delivery while the debugged one sat paused were what they were with no debugger at all.

From VS Code: open the interface's code (`interfaces/adt-in/route.ts`), set breakpoints, **Run and
Debug → Ductus: debug this interface**. VS Code starts `ductus dev --inspect` with that file's
folder (the option takes a name, or a file or folder inside the interface's folder), attaches,
and the Debug Console shows the engine's output. Send it a message (from the sender, with
`ductus gen`, or resend one it has from the web UI) and it stops at your breakpoint. Here is the
[debugging demo](#checked-in-ci) doing that through VS Code's debugger, with `other`, a second
interface, taking and delivering 100 messages while `adt` is paused, and the session then stopped
from the editor (the times at the start of each log line left out):

```text
configuration: Ductus: debug this interface: ductus dev --inspect ${fileDirname}
output: debugger listening on 127.0.0.1:9229 for adt (VS Code: "Ductus: debug this interface"; Chrome: chrome://inspect)
output: Postgres 18.6.0 is running for this project:
output:   postgresql://postgres:…@127.0.0.1:40057/ductus
output:   (data in ./.ductus/postgres/data; stops with the engine)
output: INFO ductus_runtime::worker: on a thread of its own for debugging: attach with VS Code ("Ductus: debug this interface") or chrome://inspect interface=adt debugger=ws://127.0.0.1:9229/8d316ee1-4cff-5ac2-d9ca-05e7198538bc
output: INFO ductus_runtime: started interfaces=2 mllp=2 http=0 files=0 schedules=0 internal=0 shared=1 pool_threads=8 dedicated_threads=0 debugged=1 console=info took=128.062457ms
output: WARN ductus_runtime::worker: a debugger attached: this interface's code has no time limit until it detaches (its messages wait while it's paused; every other interface goes on) interface=adt
trigger: sent 1 messages over 1 connection(s) in 0.0s (251/s), retries 0, rejected 0, refused 0, dropped 0, longest wait 3 ms
stopped: breakpoint
at: transform /tmp/tmp.wnWXPNzNZy/dev-project/interfaces/adt/route.ts:5
locals: msg=Proxy(MessageImpl) {data: Array(4), index: Map(4), delimiters: {…}}, this=undefined, ward='4W'
ward + " " + msg.get("MSH-9") = '4W ADT^A08'
while paused: sent 100 messages over 1 connection(s) in 0.1s (1325/s), retries 0, rejected 0, refused 0, dropped 0, longest wait 5 ms
while paused: other delivered 100; adt wrote 0
stopping the session
output: INFO ductus_runtime: stopping: taking no new messages, finishing the ones in hand why="SIGTERM" limit=30s
output: INFO ductus_runtime::worker: the debugger detached: the time limit (`timeout`) applies again interface=adt
output: INFO ductus_runtime: stopped took=3.835019ms
output: Postgres stopped
```

From a terminal: `ductus dev --inspect adt-in`, then attach Chrome DevTools (chrome://inspect lists
"adt-in (ductus dev)") or any other debugger on 127.0.0.1:9229. Unlike `test` and `try`, **`dev`
doesn't wait** for a debugger: the engine starts and runs as usual, and you attach when you like.

| Option | What it does |
|---|---|
| `--inspect adt-in` | the interface, by name or a file or folder inside its folder; again for another (`--inspect adt-in --inspect lab-out`) |
| `--inspect-address 127.0.0.1:9230` (or `9230`) | another port: this machine only, as for `test` |

- **An endless loop under the debugger holds only that interface.** No time limit while you're
  attached, so a loop runs on (pause it to see where); its messages wait, everything else goes on.
  The moment the debugger detaches, the limit applies again: the loop is stopped within the
  interface's `timeout`, that message quarantined ("transform stopped: the script ran longer than
  its 2s timeout"), and the messages behind it are delivered
  ([nuance](../nuances/debugging.md#under-ductus-dev---inspect-only-the-debugged-interface-waits-level-2)).
- **Stopping `dev` while paused** (the stop button, Ctrl-C) ends the debugging session itself: the
  paused call goes on and `dev` stops at once. The message it was working on is sent then if its
  code finishes before the stop reaches the workers, or else at the next start, as any message in
  hand when an engine stops: once either way
  ([nuance](../nuances/debugging.md#stopping-ductus-dev-while-paused-may-leave-that-message-for-the-next-start-level-2)).
- **Several interfaces** (`--inspect a --inspect b`) are each a target on the one port: Chrome
  DevTools lists them all; VS Code attaches to the first by name
  ([nuance](../nuances/debugging.md#with-several---inspect-interfaces-vs-code-attaches-to-the-first-level-3)).
- **A dedicated interface with several workers** runs them all on its one debugged thread for the
  run, so they take turns.
- **An interface that answers HTTP requests itself** (`respond`) can be debugged too; its callers
  wait for their answer while it's paused, and may give up by their own timeout.
- **Only in `dev`.** `ductus run` and the services have no debugger, ever.

### What it costs

Nothing you can measure. Without `--inspect`, `dev` runs as before. With it, the debugged
interface's code runs on its own thread with the debugger's hooks on, and the other interfaces
don't notice, even while it sits paused. `scripts/bench-inspect.sh` measured it on a development
machine (WSL2, `ductus dev`'s own Postgres): 20,000 messages into a second interface (with a
transform, on the shared pool) over 4 connections as fast as its ACKs came back, while the
debugged one was as the first column says; two passes of three runs each (2026-11-04):

| The debugged interface | Second interface: messages stored and ACKed per second | All 20,000 delivered after |
|---|---|---|
| no `--inspect` (the version before) | 6,103–6,790 | 10.2–10.8 s |
| no `--inspect` | 5,970–6,788 | 9.5–10.8 s |
| `--inspect`, no debugger attached | 6,659–6,975 | 9.7–10.9 s |
| `--inspect`, paused at a breakpoint the whole time | 6,404–7,052 | 9.8–11.2 s |

The differences are the spread between runs, in both directions. For scale: a busy feed sends
tens of messages a second, so this is a hundredfold more than a dev engine usually sees. In the
CLI test (20 messages a time into the second interface, each timed from send to its receiver), the
longest wait was 9–27 ms with the debugged interface paused or looping, against 22–44 ms with it
idle, and the paused interface's own senders got their ACKs in 0.5–2 ms.

`dev`'s start with `--inspect` (5 starts each, twice): the engine's own start 133–242 ms against
127–231 ms without, and from the command to "started" (its Postgres included) 335–473 ms against
325–416 ms: the same, within the spread.

## The time limit while you debug

Interface code is stopped when it runs past its `timeout` without pausing (5 s for tests), so an
endless loop can't hold a thread ([runaway scripts](../nuances/runaway.md)). Paused at a
breakpoint, your code isn't running, but V8 would see the call as a runaway the moment it went on.
So **while a debugger is attached, there's no time limit** (the memory limit stays), said when it
attaches:

```text
debugger attached: no time limit on the tests while it is
debugger detached: the time limit applies again
```

(`ductus dev --inspect` says it in its log: "a debugger attached: this interface's code has no
time limit until it detaches", "the debugger detached: the time limit (`timeout`) applies
again".)

The moment it detaches (you stop debugging, close the window, the connection drops), the limit
applies again from then: code still in an endless loop is stopped within the timeout, the run
ends and `ductus test` exits 1 (under `dev`, that message is quarantined and the interface goes
on). An endless loop under a debugger therefore can't hang anything:
pause it to see where it loops; stop debugging and it's stopped
([nuance](../nuances/debugging.md#the-time-limit-is-off-while-a-debugger-is-attached-level-2)).
A test stopped this way ends its interface's run, with or without a debugger
([nuance](../nuances/debugging.md#a-runaway-test-ends-that-interfaces-test-run-level-2)).

## `debugger;` statements

A `debugger;` line pauses there whenever a debugger is attached, like a breakpoint written in the
code; without one it does nothing (the engine never has one). One left behind pauses everyone's
debugging sessions, so `ductus validate` warns about each:

```text
interfaces/adt/route.ts:4:3 warning DX5: a `debugger;` statement: it pauses here in every debugging session (`ductus test --inspect`); remove it when you're done, and set a breakpoint in the editor instead
```

## What it doesn't do

- **Production is never debugged in place.** A paused interface would hold its real traffic, and a
  debugger's port runs any code beside patient data. Bring the message to the code instead:
  `ductus try --message ID --from https://ductus-prod:7700 --inspect` on your machine.

## Checked in CI

`scripts/demo-debug.sh` runs VS Code's JavaScript debugger (js-debug, the same code VS Code runs,
version pinned) the way VS Code does for the configuration above, on this guide's example project:
it starts `ductus test --inspect`, stops at a breakpoint on a `.ts` line through the source map,
reads the locals and an expression, and lets the test pass; then "Ductus: debug this message" on
a fixture, stopping at the same line under `ductus try`; then "Ductus: debug this interface",
read from the `launch.json` `ductus types` wrote: `ductus dev --inspect` with a message stopped at
a breakpoint while another interface takes and delivers 100, and the session stopped as VS Code's
stop button does (dev and its Postgres stop cleanly). `scripts/demo-try.sh` tries a message an
engine stored, from its database and through its API, before and after a change to the code, and
checks the receiver got nothing more. A CLI test attaches over the protocol directly, pauses at a
`debugger;`, keeps an endless loop running past the time limit while attached, and checks it's
stopped within the limit once it detaches. Another does the same under `ductus dev --inspect`
with real traffic: senders of the debugged interface ACKed while it's paused and while it loops,
a second interface's messages delivered meanwhile, the loop stopped within its 2 s limit once
the debugger detaches (2.05 s), its message quarantined and the five behind it delivered, and
`dev` stopped while paused without waiting. Chrome DevTools was checked by hand (2026-11-03): it
lists the run, stops at `debugger;` in the `.ts` with its types and locals, and resumes.
