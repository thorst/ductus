# Debugging: breakpoints in your code

When a test fails and the reason isn't obvious from the values it prints, stop the code where it
goes wrong and look: the values of every variable, the call that led there, what an expression
gives, one line at a time. `ductus test --inspect` runs an interface's tests under a debugger.

**No extension is needed.** The engine speaks the protocol every JavaScript debugger speaks (the
Chrome DevTools Protocol, as Node and Deno do), so VS Code's built-in debugger, Chrome DevTools,
JetBrains IDEs and anything else that can attach to Node attach to Ductus too.

| You use | What to do |
|---|---|
| **VS Code** | open a file of the interface (its code or a test), set breakpoints, Run and Debug → **Ductus: debug these tests** ([below](#vs-code)) |
| **Chrome or Edge** | `ductus test --inspect --interface adt-in` in a terminal, then chrome://inspect → **inspect** ([below](#chrome-devtools)) |
| **JetBrains, Neovim, others** | `ductus test --inspect`, then attach to a Node.js process on 127.0.0.1:9229 ([below](#other-debuggers)) |

## VS Code

`ductus types` (also run by every `ductus test` and `ductus validate`) adds a configuration to
`.vscode/launch.json`: only added, beside your own, and never over one of the same name.

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
    }
  ]
}
```

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

The moment it detaches (you stop debugging, close the window, the connection drops), the limit
applies again from then: code still in an endless loop is stopped within the timeout, the run
ends and `ductus test` exits 1. An endless loop under a debugger therefore can't hang anything:
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

## What it doesn't do (yet)

- **One message through an interface** under the debugger (`ductus try`), and **a running dev
  engine** with one interface debugged (`ductus dev --inspect`), are the next parts of v0.7.
- **Production is never debugged in place.** A paused interface would hold its real traffic, and a
  debugger's port runs any code beside patient data. Bring the message to the code instead: a
  fixture made from it (with the patient replaced), or, once it ships, `ductus try` with the stored
  message.

## Checked in CI

`scripts/demo-debug.sh` runs VS Code's JavaScript debugger (js-debug, the same code VS Code runs,
version pinned) the way VS Code does for the configuration above, on this guide's example project:
it starts `ductus test --inspect`, stops at a breakpoint on a `.ts` line through the source map,
reads the locals and an expression, and lets the test pass. A CLI test attaches over the protocol directly, pauses at a
`debugger;`, keeps an endless loop running past the time limit while attached, and checks it's
stopped within the limit once it detaches. Chrome DevTools was checked by hand (2026-11-03): it
lists the run, stops at `debugger;` in the `.ts` with its types and locals, and resumes.
