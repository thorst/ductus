# Ductus for VS Code

Your [Ductus](https://getductus.com) interfaces' tests in VS Code's Test Explorer, run and debugged
one test at a time; `ductus validate`'s findings in Problems as you save; debugging without a
`launch.json`; your change deployed to your `ductus dev` in place; a release promoted from test
to prod; each interface's history beside its code; and a message an engine stored, debugged from
the web UI in your copy of the code. The extension runs the `ductus` program you already use (its `--json` outputs), so
what it shows is what `ductus test` and `ductus validate` say in a terminal or in CI.

## What it does

| Where                                                                                         | What you get                                                                                                                                                                                                                                                                                                                                                                                                         |
| --------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Testing** (the flask icon)                                                                  | every interface's tests, by interface, found by `ductus test --list`. Run or debug one test, one interface or everything, from the tree or the ▷ beside each `test(...)` in the file. Each result shows the moment its test ends; a failure sits on its `expect` line with the expected and actual values side by side (**Compare**). A test that runs away (an endless loop) fails by name, and the rest still run. |
| **Problems**                                                                                  | `ductus validate`'s findings (configuration, wiring, your code's Ductus rules), each at its key in the YAML or its place in the code, when the workspace opens and each time you save a file of the project. TypeScript's own errors are left to VS Code's TypeScript, so nothing shows twice.                                                                                                                       |
| **Run and Debug**                                                                             | **Ductus: debug these tests**, **Ductus: debug this message** (with a fixture open) and **Ductus: debug this interface** (`ductus dev`) with no `launch.json`: pick one from the list (under **Ductus**), or press F5 with a file of an interface open. Breakpoints are on your `.ts` lines.                                                                                                                         |
| **Deploy to dev** (the ☁ button above the editor)                                             | while your `ductus dev` runs for the project: it takes the project as it is on disk, in place (the interfaces you changed take it between two messages, the others never pause). Found and signed in to by itself: nothing to configure. On every save too with `ductus.deployToDevOnSave`.                                                                                                                          |
| **Promote…** (Command Palette)                                                                | a release from the project's release files, or from the engine it's on, to another engine named in `ductus.engines`, with the reason: `ductus deploy` runs it, the engine's gate and rules decide, and the report (the gate's findings, what changed) is in the output. Each engine's token is asked for once and kept in your system's keychain.                                                                    |
| **Ductus History** (in the Explorer)                                                          | who changed the open file's interface, what and why, newest first (commits, deployments, the audit log, documentation), as `ductus history` shows it: from your running `ductus dev`, an engine named in `ductus.engines`, or the folder. It reads only while the view is open.                                                                                                                                      |
| **Debug in VS Code** (a message's page in the web UI), or **Ductus: Debug a stored message…** | the message read from its engine (a token with `payloads`; the read is in its audit log) and run through your copy of the code under the debugger, stopping at your breakpoints: `ductus try --message --from --inspect`. Nothing is sent. A link goes on by itself only to your `ductus dev` or an engine you named.                                                                                                |

## What it needs

- **`ductus` 0.7.0 or newer** on the `PATH` (the installers put it there), or set `ductus.path`.
  The extension says so, with a link to the download, when it can't find one or finds an older one.
- A **Ductus project** in the workspace: a folder with `interfaces/<name>/interface.yaml`. The
  extension starts only then (or when a Ductus debug configuration is started).
- VS Code 1.95 or newer. The project in WSL, a container or over SSH: open it there (Remote
  Development) so the extension runs where `ductus` is.

## Settings

| Setting                    | Default                       | What it does                                                                                                                                   |
| -------------------------- | ----------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------- |
| `ductus.path`              | empty: `ductus` on the `PATH` | the program to run; relative to the workspace folder, or with `${workspaceFolder}`                                                             |
| `ductus.environment`       | `dev`                         | the environment the project is read in (`--env`) for tests, checks and debugging                                                               |
| `ductus.validateOnSave`    | `true`                        | run `ductus validate` on save (once for several saves close together). Off: when the workspace opens and with **Ductus: Validate the project** |
| `ductus.deployToDevOnSave` | `false`                       | Deploy to dev on save, while the project's `ductus dev` runs                                                                                   |
| `ductus.engines`           | none                          | the engines Promote…, History and Debug this message use, by name: `{ "test": "https://ductus-test:7700" }` (tokens are never kept here)       |

## How it works, and what it costs

- **Tests**: `ductus test --list --json` when the workspace opens (and again when a test file or an
  interface.yaml is saved); a run is `ductus test --json` with `--interface` and `--test` for what
  you chose. A debugged run adds `--inspect` on a free port and attaches VS Code's own JavaScript
  debugger; one interface is debugged at a time (choosing several debugs them one after another).
- **Problems**: `ductus validate --json` per project, at most one at a time, about a second on a
  small project (most of it the type check), in a process of its own: the editor never waits for it.
- **Deploy to dev and Promote…** call the engine's API (Deploy to dev) and run `ductus deploy
--json` (Promote…) only when you ask, or on save with the setting.
- **History** reads a page (`GET /interfaces/{name}/history`, or `ductus history --json` from the
  folder) when the view is open and the open file's interface changes; hidden, it reads nothing.
  **Debug this message** runs `ductus try` under the debugger only when you ask.
- **Nothing runs in the background** otherwise: no timers, no watchers beyond VS Code's own file
  events. The output of every command the extension runs, with how long it took, is in
  **View → Output → Ductus**.

`ductus test` and `ductus validate` write the editor typings (`.ductus/`) as they always do, only
when they changed.

## More

- [Testing with fixtures](https://github.com/thorst/ductus/blob/main/docs/write/testing.md)
- [Debugging: breakpoints in your code](https://github.com/thorst/ductus/blob/main/docs/write/debugging.md)
- [validate's findings](https://github.com/thorst/ductus/blob/main/docs/reference/validate.md)
- [The extension's guide: Deploy to dev, Promote…, History, Debug this message](https://github.com/thorst/ductus/blob/main/docs/write/vscode.md)
- [Releases and promotion](https://github.com/thorst/ductus/blob/main/docs/operate/releases.md)

Apache-2.0, part of [the Ductus repository](https://github.com/thorst/ductus) (`editors/vscode`).
