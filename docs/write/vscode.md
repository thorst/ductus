# In VS Code: the Ductus extension

The [testing](testing.md) and [debugging](debugging.md) pages work from a terminal and with VS
Code's own debugger, no extension needed. The Ductus extension puts the same commands where you
already look in VS Code:

| Where | What you get |
|---|---|
| **Testing** (the flask in the activity bar) | every interface's tests, by interface, each at its `test(...)` line with a ▷ beside it in the file. Run or debug one test, one interface or everything. Each result shows the moment its test ends; a failure sits on its `expect` line with the expected and actual values side by side |
| **Problems** | [`ductus validate`](../reference/validate.md)'s findings at their key in the YAML or their place in the code, when the workspace opens and each time you save a file of the project |
| **Run and Debug** | **Ductus: debug these tests**, **Ductus: debug this message** and **Ductus: debug this interface** with no `launch.json`, or F5 with a file of an interface open |
| **Deploy to dev** (the ☁ button above the editor while your `ductus dev` runs) | your change running in your [`ductus dev`](../install/dev.md), in place: the interfaces you changed take it, the others never pause |
| **Promote…** (Command Palette) | a release to test or prod, from the project's release files or from the engine it's on, with the gate's report and the outcome, as [`ductus deploy`](../operate/releases.md) does it |

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

## Deploy to dev

With [`ductus dev`](../install/dev.md) running for the project (in VS Code's terminal or any
other), **Ductus: Deploy to dev** (Command Palette, or the ☁ button above the editor) makes it run
the project as it is now on disk, as `ductus reload` in the project's folder does: the interfaces
whose code or interface.yaml changed take it between two messages, added ones start, removed ones
stop, every other interface goes on untouched. A notification says which took it and how long it
took (`Deployed to dev: adt-in in 84 ms`); each step and code version is in the output.

It finds the engine and signs in by itself, through the two files `ductus dev` keeps in `.ductus/`
([how](../install/dev.md#how-ductus-reload-and-editors-find-it)): nothing to configure, no token to
paste. With no `ductus dev` running it says so, with a button that starts one in a terminal.

Refused, with nothing changed and the reason in the notification: a project that doesn't load
(the same errors as Problems), a change only a start applies (ductus.yaml, resources.yaml: restart
`ductus dev`), and a change to an interface you're debugging with **Ductus: debug this interface**
(restart the debug session)
([nuance](../nuances/dev.md#ductus-dev-reloads-in-place-only-level-1)).

**On every save**: turn on `ductus.deployToDevOnSave`. Several saves close together deploy once;
a success shows in the status bar instead of a notification, a refusal still notifies. While no
`ductus dev` runs, saving does nothing more than before.

## Promote…

Moving a release on, from your release file to test or from test to prod, without leaving the
editor. It's [`ductus deploy`](../reference/cli/deploy.md) underneath, so everything the
[releases](../operate/releases.md) page says holds: each engine holds the release to its
environment's rules and to the "replaces" check, runs the gate, and applies it in place when it
can.

First, name your engines once in the setting `ductus.engines` (in the workspace's
`.vscode/settings.json` to share it with the team, since it holds no secrets):

```json
{
  "ductus.engines": {
    "test": "https://ductus-test:7700",
    "prod": "https://ductus-prod:7700"
  }
}
```

Then **Ductus: Promote…** (Command Palette) asks four things:

| Step | You choose | |
|---|---|---|
| 1 | where the release is | **Release files in this project** (what [`ductus release`](../reference/cli/release.md) wrote into `.ductus/releases`), or **On test** (an engine's stored releases) |
| 2 | which release | newest first; an engine's show who made them, the branch and where they've been deployed, with ▷ on the one it runs ([nuance](../nuances/releases.md#promote-lists-an-engines-releases-of-the-project-with-the-folders-name-level-2)) |
| 3 | the engine it goes to | any named; the one it's stored on too (a release CI stored there, deployed from here) |
| 4 | why | recorded with the deployment, in the interface's history and the audit log; required except for an environment named `dev`. **Enter deploys it** |

A notification stays until the engine answers
([nuance](../nuances/releases.md#promote-waits-for-the-engines-answer-closing-vs-code-doesnt-stop-the-deploy-level-1)),
then says how it went: `Promoted 0a1b2c3d4e5f to prod (prod): adt-in (in place)`, or `Promote to
prod (prod) refused:` and the first reason (a rule such as "takes only releases deployed to test
first", someone else's work it would undo, the gate's first problem). **Show the report** opens
the whole of it in the output: the rules, the gate's findings, what changed in each interface,
each step.

What it runs, in the project's folder, with the tokens in its environment (never on the command
line):

| From | It runs |
|---|---|
| a release file | `ductus deploy .ductus/releases/integrations-0a1b2c3d4e5f.tar.gz --to https://ductus-test:7700 -m "…" --json` (the file then records where it went, as in a terminal) |
| an engine | `ductus deploy 0a1b2c3d4e5f --from https://ductus-test:7700 --to https://ductus-prod:7700 -m "…" --json` |

**Tokens.** The first time an engine needs one, a password box asks for it: a token with the
`deploy` permission for the engine it goes to, one that can fetch releases (`download`, or an
operator's `deploy`) for the engine it comes from. Make your own on the web UI's **Account** page
(**Your tokens**), so the audit log names you. Each is checked on its engine before it's used, and
kept in your system's keychain
([nuance](../nuances/releases.md#vs-code-keeps-each-engines-token-in-your-systems-keychain-by-its-url-level-1)):
the next promotion asks nothing. **Ductus: Forget an engine's token** removes one.

**When not:** an engine running a project folder rather than releases (`ductus run` without
`--releases`) takes no deploys, and Promote… says so; for your own `ductus dev`, use [Deploy to
dev](#deploy-to-dev). A rollback, `--force` over someone else's work and `--interface` (some
interfaces only) are in a terminal, with `ductus deploy`, or on the engine's Releases page.

## Settings

| Setting | Default | What it does |
|---|---|---|
| `ductus.path` | empty: `ductus` on the `PATH` | the program to run, as a full path, relative to the workspace folder, or with `${workspaceFolder}` |
| `ductus.environment` | `dev` | the environment (`--env`) tests, checks and debugging read the project in |
| `ductus.validateOnSave` | `true` | `validate` after each save; off, only when the workspace opens and with **Ductus: Validate the project** (Command Palette) |
| `ductus.deployToDevOnSave` | `false` | [Deploy to dev](#deploy-to-dev) after each save, while the project's `ductus dev` runs |
| `ductus.engines` | none | the engines [Promote…](#promote) deploys to and fetches from, by name: `{ "test": "https://ductus-test:7700" }` (tokens are never kept here) |

## What it costs

Measured on the guides' example project in VS Code 1.147 (the [CI demo](#checked-in-ci)), on a
developer's workstation in WSL:

| | |
|---|---|
| the extension's start | 1.5 ms: it finds the projects and lists the tests after it has started, never in VS Code's way |
| its package | 22 KB: one bundled file, nothing else to download |
| an interface's tests from the Test Explorer (`adt-in`'s 5) | 60–75 ms, as `ductus test` takes in a terminal |
| a save to its finding in Problems | 1.25 s (the debounce, 0.4 s, and `validate`, most of it the type check) |
| Deploy to dev of one interface | 23–42 ms from the command to the answer |
| Promote… of a one-interface release (a file to test; test to prod) | 1.2–1.3 s from **Enter** to the answer, most of it the gate (its tests and `validate` on the engine's machine); a larger project's gate takes longer ([measured](../operate/releases.md#measured)) |

Nothing runs in the background besides: no timers or polling. Every command it runs, and how long
it took, is in **View → Output → Ductus**; when something didn't run (no `ductus`, a project that
doesn't load), that's where it says why.

## Checked in CI

`scripts/demo-vscode.sh` starts a real VS Code (under a virtual display) with the extension on a
copy of this guide's [example project](https://github.com/thorst/ductus/tree/main/examples/guides)
and the build being tested: the tests found at their lines, an interface's tests run, a failing
test shown at its `expect` with both values, a port mistyped in interface.yaml shown in Problems on
save and gone once fixed, a breakpoint in `lab.ts` hit from the Test Explorer's **Debug**,
**Ductus: debug these tests** with no `launch.json`, **Deploy to dev** against a real `ductus
dev`, by the command and on save, and **Promote…** across two engines in release mode (test, and
prod requiring test first): a release file refused by prod's rules, deployed to test, then
promoted from test to prod, each token asked for once and a viewer's turned away before anything
ran.
