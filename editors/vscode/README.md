# Ductus for VS Code

Your [Ductus](https://getductus.com) interfaces' tests in VS Code's Test Explorer, run and debugged
one test at a time; `ductus validate`'s findings in Problems as you save; and debugging without a
`launch.json`. The extension runs the `ductus` program you already use (its `--json` outputs), so
what it shows is what `ductus test` and `ductus validate` say in a terminal or in CI.

## What it does

| Where                        | What you get                                                                                                                                                                                                                                                                                                                                                                                                         |
| ---------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Testing** (the flask icon) | every interface's tests, by interface, found by `ductus test --list`. Run or debug one test, one interface or everything, from the tree or the ▷ beside each `test(...)` in the file. Each result shows the moment its test ends; a failure sits on its `expect` line with the expected and actual values side by side (**Compare**). A test that runs away (an endless loop) fails by name, and the rest still run. |
| **Problems**                 | `ductus validate`'s findings (configuration, wiring, your code's Ductus rules), each at its key in the YAML or its place in the code, when the workspace opens and each time you save a file of the project. TypeScript's own errors are left to VS Code's TypeScript, so nothing shows twice.                                                                                                                       |
| **Run and Debug**            | **Ductus: debug these tests**, **Ductus: debug this message** (with a fixture open) and **Ductus: debug this interface** (`ductus dev`) with no `launch.json`: pick one from the list (under **Ductus**), or press F5 with a file of an interface open. Breakpoints are on your `.ts` lines.                                                                                                                         |

## What it needs

- **`ductus` 0.7.0 or newer** on the `PATH` (the installers put it there), or set `ductus.path`.
  The extension says so, with a link to the download, when it can't find one or finds an older one.
- A **Ductus project** in the workspace: a folder with `interfaces/<name>/interface.yaml`. The
  extension starts only then (or when a Ductus debug configuration is started).
- VS Code 1.95 or newer. The project in WSL, a container or over SSH: open it there (Remote
  Development) so the extension runs where `ductus` is.

## Settings

| Setting                 | Default                       | What it does                                                                                                                                   |
| ----------------------- | ----------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------- |
| `ductus.path`           | empty: `ductus` on the `PATH` | the program to run; relative to the workspace folder, or with `${workspaceFolder}`                                                             |
| `ductus.environment`    | `dev`                         | the environment the project is read in (`--env`) for tests, checks and debugging                                                               |
| `ductus.validateOnSave` | `true`                        | run `ductus validate` on save (once for several saves close together). Off: when the workspace opens and with **Ductus: Validate the project** |

## How it works, and what it costs

- **Tests**: `ductus test --list --json` when the workspace opens (and again when a test file or an
  interface.yaml is saved); a run is `ductus test --json` with `--interface` and `--test` for what
  you chose. A debugged run adds `--inspect` on a free port and attaches VS Code's own JavaScript
  debugger; one interface is debugged at a time (choosing several debugs them one after another).
- **Problems**: `ductus validate --json` per project, at most one at a time, about a second on a
  small project (most of it the type check), in a process of its own: the editor never waits for it.
- **Nothing runs in the background** otherwise: no timers, no watchers beyond VS Code's own file
  events. The output of every command the extension runs, with how long it took, is in
  **View → Output → Ductus**.

`ductus test` and `ductus validate` write the editor typings (`.ductus/`) as they always do, only
when they changed.

## More

- [Testing with fixtures](https://github.com/thorst/ductus/blob/main/docs/write/testing.md)
- [Debugging: breakpoints in your code](https://github.com/thorst/ductus/blob/main/docs/write/debugging.md)
- [validate's findings](https://github.com/thorst/ductus/blob/main/docs/reference/validate.md)

Apache-2.0, part of [the Ductus repository](https://github.com/thorst/ductus) (`editors/vscode`).
