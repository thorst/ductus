# In VS Code: the Ductus extension

The [testing](testing.md) and [debugging](debugging.md) pages work from a terminal and with VS
Code's own debugger, no extension needed. The Ductus extension puts the same commands where you
already look in VS Code:

| Where | What you get |
|---|---|
| **Testing** (the flask in the activity bar) | every interface's tests, by interface, each at its `test(...)` line with a ▷ beside it in the file. Run or debug one test, one interface or everything. Each result shows the moment its test ends; a failure sits on its `expect` line with the expected and actual values side by side |
| **Problems** | [`ductus validate`](../reference/validate.md)'s findings at their key in the YAML or their place in the code, when the workspace opens and each time you save a file of the project |
| **Run and Debug** | **Ductus: debug these tests**, **Ductus: debug this message** and **Ductus: debug this interface** with no `launch.json`, or F5 with a file of an interface open |

It runs the `ductus` you already have, through the same `--json` outputs that tools and CI read
([for editors and CI](testing.md#for-editors-and-ci-json)), so what it shows is what `ductus test`
and `ductus validate` say in a terminal: nothing is checked twice by different code.

## Install

- **`ductus` 0.7.0 or newer** on the `PATH` (the [installers](../install/index.md) put it there),
  or the setting `ductus.path`. Without one, or with an older one, the extension says so with a
  link to the download, and its views stay empty.
- **The extension**: in VS Code's Extensions view, search **Ductus** (published with each release
  from 0.7.0). Until then, or for one you built: `npm ci && npm run package` in `editors/vscode`
  of the [repository](https://github.com/thorst/ductus) makes `dist/ductus.vsix`; Extensions →
  **…** → **Install from VSIX…**.
- **A project in WSL, a container or over SSH**: open it there with VS Code's Remote
  Development, so the extension runs where `ductus` and the project are.

It starts when the workspace has a Ductus project (a folder with
`interfaces/<name>/interface.yaml`), or when a Ductus debug configuration starts. A workspace with
several projects shows each in the Test Explorer under its folder.

## Tests

Open the Test Explorer (the flask): each interface with tests is there, its tests under it. Click
▷ beside a test, an interface or at the top to run them; the bug icon beside it (or **Debug
Test** on the ▷ in the file) runs it under the debugger and stops at your breakpoints, in the
code under test or in the test itself.

A failing test shows on the `expect` that failed, with what was expected and what came instead;
**Compare** opens them side by side (objects as indented JSON). A test that [runs
away](testing.md#when-a-test-runs-away) fails by name with the reason, and the rest still run.
What the code wrote with `console` is in **Test Results** under the test that wrote it.

What each action runs, in the project's folder:

| You choose | It runs |
|---|---|
| everything | `ductus test --json --env dev` |
| an interface (or several) | `ductus test --json --env dev --interface adt-in` |
| some of an interface's tests | `ductus test --json --env dev --interface adt-in --test "the lab gets visits, …"` |
| **Debug** | the same with `--inspect=127.0.0.1:<a free port>`, one interface at a time ([nuance](../nuances/debugging.md#debugging-several-interfaces-from-the-test-explorer-runs-them-one-after-another-level-1)) |

The tree is what `ductus test --list --json` found when the workspace opened, and again after you
save a test file or an interface.yaml
([nuance](../nuances/transforms.md#vs-codes-test-explorer-shows-the-tests-as-they-were-last-listed-level-1)).

## Problems

`ductus validate --json` runs when the workspace opens and after you save a file of the project
(once for several saves close together, never two at a time for one project; about a second on a
small project, most of it the type check). Its findings replace the last run's: fix the port you
mistyped, save, and the error goes. Each has its code (`DUC122`), whose meaning and fix are in the
[findings reference](../reference/validate.md). TypeScript's own errors aren't repeated: VS Code
shows those as you type
([nuance](../nuances/transforms.md#problems-shows-ductuss-findings-typescripts-come-from-vs-code-itself-level-1)).

## Debugging without `launch.json`

The [three configurations](debugging.md#vs-code) `ductus types` writes into `launch.json` are
also the extension's own: in **Run and Debug**, open the list and choose **Ductus** → one of them,
or press F5 with nothing configured and a file of an interface open (a fixture runs as **debug
this message**, anything else as **debug these tests**). They run the `ductus` the extension
found, in the open file's project, on a free port, so a `ductus dev` already on 9229 is no
obstacle.

To keep one with your own options, add it to `launch.json` with the type `ductus`:

```json
{
  "type": "ductus",
  "request": "launch",
  "name": "A31 only",
  "command": "test",
  "args": ["--filter", "A31"]
}
```

| Field | What it is | Default |
|---|---|---|
| `command` | `test` (the open file's interface's tests), `try` (the open file as a message through its interface) or `dev` (a dev engine with the open file's interface under the debugger) | `test` |
| `file` | a file of the interface (for `try`, the message); `${file}`, `${fileDirname}` and `${workspaceFolder}` are filled in | the open file |
| `args` | more options for the command | none |

## Settings

| Setting | Default | What it does |
|---|---|---|
| `ductus.path` | empty: `ductus` on the `PATH` | the program to run, as a full path, relative to the workspace folder, or with `${workspaceFolder}` |
| `ductus.environment` | `dev` | the environment (`--env`) tests, checks and debugging read the project in |
| `ductus.validateOnSave` | `true` | `validate` after each save; off, only when the workspace opens and with **Ductus: Validate the project** (Command Palette) |

## What it costs

Measured on the guides' example project in VS Code 1.147 (the [CI demo](#checked-in-ci)), on a
developer's workstation in WSL:

| | |
|---|---|
| the extension's start | 1.5 ms: it finds the projects and lists the tests after it has started, never in VS Code's way |
| its package | 16 KB: one bundled file, nothing else to download |
| an interface's tests from the Test Explorer (`adt-in`'s 5) | 60–75 ms, as `ductus test` takes in a terminal |
| a save to its finding in Problems | 1.25 s (the debounce, 0.4 s, and `validate`, most of it the type check) |

Nothing runs in the background besides: no timers or polling. Every command it runs, and how long
it took, is in **View → Output → Ductus**; when something didn't run (no `ductus`, a project that
doesn't load), that's where it says why.

## Checked in CI

`scripts/demo-vscode.sh` starts a real VS Code (under a virtual display) with the extension on a
copy of this guide's [example project](https://github.com/thorst/ductus/tree/main/examples/guides)
and the build being tested: the tests found at their lines, an interface's tests run, a failing
test shown at its `expect` with both values, a port mistyped in interface.yaml shown in Problems on
save and gone once fixed, a breakpoint in `lab.ts` hit from the Test Explorer's **Debug**, and
**Ductus: debug these tests** with no `launch.json`.
