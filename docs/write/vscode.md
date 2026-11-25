# In VS Code: the Ductus extension

The [testing](testing.md) and [debugging](debugging.md) pages work from a terminal and with VS
Code's own debugger, no extension needed. The Ductus extension puts the same commands where you
already look in VS Code:

| Where | What you get |
|---|---|
| **Testing** (the flask in the activity bar) | every interface's tests, by interface, each at its `test(...)` line with a ▷ beside it in the file. Run or debug one test, one interface or everything. Each result shows the moment its test ends; a failure sits on its `expect` line with the expected and actual values side by side |
| **Problems** | [`ductus validate`](../reference/validate.md)'s findings at their key in the YAML or their place in the code, when the workspace opens and each time you save a file of the project |
| **The code and interface.yaml together** | above each function an interface runs, which interfaces and destinations run it (`used by adt-in → lab`); F12 on a `transform:` or `script:` value in interface.yaml goes to the function; a `script:` path opens its file |
| **Run and Debug** | **Ductus: debug these tests**, **Ductus: debug this message** and **Ductus: debug this interface** with no `launch.json`, or F5 with a file of an interface open |
| **Deploy to dev** (the ☁ button above the editor while your `ductus dev` runs) | your change running in your [`ductus dev`](../install/dev.md), in place: the interfaces you changed take it, the others never pause |
| **Promote…** (Command Palette) | a release to test or prod, from the project's release files or from the engine it's on, with the gate's report and the outcome, as [`ductus deploy`](../operate/releases.md) does it |
| **Ductus History** (in the Explorer) | who changed the open file's interface, what and why, newest first: commits, deployments, the audit log, its documentation, as [`ductus history`](../operate/deployments.md) shows it, from your `ductus dev`, an engine or the folder |
| **Debug in VS Code** (a message's page in the web UI), or **Ductus: Debug a stored message…** | a message an engine stored, run through your copy of the code under the debugger, stopping at your breakpoints; nothing is sent |

It runs the `ductus` you already have, through the same `--json` outputs that tools and CI read
([for editors and CI](testing.md#for-editors-and-ci-json)), so what it shows is what `ductus test`
and `ductus validate` say in a terminal: nothing is checked twice by different code.

## Install

- **`ductus` 0.7.0 or newer** on the `PATH` (the [installers](../install/index.md) put it there),
  or the setting `ductus.path`. Without one, or with an older one, the extension says so with a
  link to the download, and its views stay empty.
- **The extension**: download the `.vsix` from the
  [release's page](https://github.com/thorst/ductus/releases) (beside the engine's files, the
  same version), then in VS Code's Extensions view **…** → **Install from VSIX…** and pick it.
  A `.vsix` doesn't update itself: install the next release's the same way when you upgrade
  `ductus`. It's not yet in the VS Code Marketplace or Open VSX; when it is, under the publisher
  `getductus`, it's the same extension (`getductus.ductus`), so the published one replaces the
  one you installed by hand. One you built yourself: `npm ci && npm run package` in
  `editors/vscode` of the [repository](https://github.com/thorst/ductus) makes
  `dist/ductus.vsix`.
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
(once for several saves close together, never two at a time for one project; under a second on a
small project). Its findings replace the last run's: fix the port you
mistyped, save, and the error goes. Each has its code (`DUC122`), whose meaning and fix are in the
[findings reference](../reference/validate.md). TypeScript's own errors aren't repeated: VS Code
shows those as you type
([nuance](../nuances/transforms.md#problems-shows-ductuss-findings-typescripts-come-from-vs-code-itself-level-1)).

## The code and interface.yaml together

interface.yaml says which function each destination runs; VS Code's TypeScript sees only an
exported function nobody imports. The extension joins the two:

- **Above each function an interface runs**, a line says who runs it: `used by adt-in → lab`, or
  for a function in a shared file several interfaces use, `used by adt-in → billing, lab · orders
  → lab` (an HTTP interface in `respond` mode, which answers from its `script`'s `transform`,
  shows by its name alone). Click it to
  see those places in interface.yaml. A function no interface runs (a helper, one a test calls)
  gets no line.
- **F12** (or **Go to Definition**, or Ctrl+click) in interface.yaml goes to the function:
  on `toBilling` in `transform: toBilling`, on `lab.ts` in `script: lab.ts` (to the `transform`
  in it), or on a destination's name (to the `transform` it runs from the interface's `script`).
  It follows a re-export (`export { toBilling } from "../shared/billing.ts"`) to the shared file.
- **A `script:` path** is a link: Ctrl+click it to open the file.

```yaml
# interfaces/adt-in/interface.yaml, in part
destinations:
  billing:
    http: { api: billing }
    transform: toBilling      # F12 here: shared/billing.ts, at `export function toBilling`
```

They come from the same `ductus validate --json` run as Problems (its
[`uses`](testing.md#ductus-validate---json)), so nothing else runs for them: they're as of the
last save. A file with unsaved changes shows none until you save it, since its lines may have
moved ([nuance](../nuances/transforms.md#vs-codes-used-by-lines-and-go-to-definition-are-as-of-the-last-save-level-1)).
With `ductus.validateOnSave` off they're as of the last **Ductus: Validate the project**.

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

## History

**Ductus History** in the Explorer shows the [history](../operate/deployments.md) of the
interface whose file is open, newest first, and follows the editor: open a file of another
interface and it shows that one's. Each line is one entry with its icon (a commit, a deployment,
the audit log, documentation), who and when, and the parts it touches (`lab`, `source`); point at
it for the details (the commit's files, the deployment's reason and code versions, the fields that
changed). **Older…** at the end reads the next 50.

Where it reads from, chosen with the button at the top of the view (kept per project):

| From | What it shows | Read with |
|---|---|---|
| **Your ductus dev** (while it runs: the default then) | its deployments, including each **Deploy to dev**, the audit log, and the commits | `GET /interfaces/{name}/history`, signed in by itself |
| an engine named in `ductus.engines` | what that engine recorded: who deployed what to test or prod, and why | the same, with a token of yours (any: it only reads), asked once and kept in your keychain |
| **This folder** (the default with no dev running) | the commits on the interface's files | `ductus history <interface> --json` in the project's folder; the database too when `DUCTUS_DATABASE_URL` is set where VS Code runs |

What the history lacks is said at the top of the view ("the project isn't in a git repository: no
commits"). It reads only while the view is open: hidden, it does nothing, and an editor change
reads again only when you look.

## Debug this message

A message that went wrong on test or prod, run through your copy of the code with breakpoints:

1. Set a breakpoint in the code that handles it (interface.yaml names the files).
2. On the message's page in the web UI, click **Debug in VS Code**. Or, in VS Code, **Ductus:
   Debug a stored message…** (Command Palette) with a file of its interface open: choose the
   engine it's on and type its id.
3. VS Code asks whether to let the extension open the link, then runs, in the project's folder:

   ```text
   ductus try adt-in --message 20042988 --from https://ductus-prod:7700 --inspect=127.0.0.1:<a free port> --env dev
   ```

   with the token in its environment (never on the command line), and attaches the debugger. It
   stops at your breakpoint; when it ends, the Debug Console has what `ductus try` says: what each
   destination would do with it now, beside what it did then
   ([`ductus try`](debugging.md#one-message-ductus-try)).

**Nothing is sent**, and the code is *yours*, as it is on disk now, read in `ductus.environment`:
not the engine's
([nuance](../nuances/debugging.md#debug-this-message-runs-your-copy-of-the-code-not-the-engines-level-1)).
Reading the message needs a token with the `payloads` permission, asked for once per engine and
kept in your keychain (your `ductus dev`'s own has it), and the read is in that engine's audit log
as any payload read is. The web UI shows the button only to those who may read the payload.

With no breakpoint anywhere in the project, it opens the interface's interface.yaml and asks you to
set one first (**Debug without one** runs it through). The link names the engine, the interface
and the message; it opens in the project that has that interface (several: you choose).

**A link only goes to engines you know**: your `ductus dev`, or one named in `ductus.engines`
(`localhost` and `127.0.0.1` count as the same). A link to any other engine asks first, showing its
address, and adds it to your user settings under the name you give
([nuance](../nuances/debugging.md#a-debug-in-vs-code-link-opens-only-engines-you-named-level-2)).

## Settings

| Setting | Default | What it does |
|---|---|---|
| `ductus.path` | empty: `ductus` on the `PATH` | the program to run, as a full path, relative to the workspace folder, or with `${workspaceFolder}` |
| `ductus.environment` | `dev` | the environment (`--env`) tests, checks and debugging read the project in |
| `ductus.validateOnSave` | `true` | `validate` after each save; off, only when the workspace opens and with **Ductus: Validate the project** (Command Palette). Problems and the [`used by` lines](#the-code-and-interfaceyaml-together) are as of that run |
| `ductus.deployToDevOnSave` | `false` | [Deploy to dev](#deploy-to-dev) after each save, while the project's `ductus dev` runs |
| `ductus.engines` | none | the engines [Promote…](#promote) deploys to and fetches from, [History](#history) and [Debug this message](#debug-this-message) read from, by name: `{ "test": "https://ductus-test:7700" }` (tokens are never kept here) |

## What it costs

Measured on the guides' example project in VS Code 1.147 (the [CI demo](#checked-in-ci)), on a
developer's workstation in WSL:

| | |
|---|---|
| the extension's start | 1.5–6 ms: it finds the projects and lists the tests after it has started, never in VS Code's way |
| its package | 29 KB: one bundled file, nothing else to download |
| an interface's tests from the Test Explorer (`adt-in`'s 5) | 60–75 ms, as `ductus test` takes in a terminal |
| a save to its finding in Problems | 1.25–1.87 s (the debounce, 0.4 s, and `validate`), measured when `validate` took ~1 s on this project; with TypeScript 7's `tsc` (0.7.9.2) it takes 0.3–0.6 s |
| a file's `used by` lines | under 1 ms: read from the last `validate`, nothing run (the `validate` itself took as long with them as without: 0.7 s on this project, 3.6 s at 1,001 interfaces) |
| Deploy to dev of one interface | 23–42 ms from the command to the answer |
| Promote… of a one-interface release (a file to test; test to prod) | about 0.5 s from **Enter** to the answer (493–497 ms; 1.2–1.4 s before validate type-checked with TypeScript 7's `tsc`), most of it the gate (its tests and `validate` on the engine's machine); a larger project's gate takes longer ([measured](../operate/releases.md#measured)) |
| an interface's History (a page of 50) | 14 ms from a `ductus dev`; 233 ms from the folder (`ductus history` starting and reading git) |
| Debug this message, from the link to the end of the session (one breakpoint, continued at once) | 1.6 s: `ductus try` starting, the debugger attaching, the message read through the API |

Nothing runs in the background besides: no timers or polling. Every command it runs, and how long
it took, is in **View → Output → Ductus**; when something didn't run (no `ductus`, a project that
doesn't load), that's where it says why.

## Checked in CI

`scripts/demo-vscode.sh` starts a real VS Code (under a virtual display) with the extension on a
copy of this guide's [example project](https://github.com/thorst/ductus/tree/main/examples/guides)
and the build being tested: the tests found at their lines, an interface's tests run, a failing
test shown at its `expect` with both values, a port mistyped in interface.yaml shown in Problems on
save and gone once fixed, `lab.ts`'s `used by adt-in → lab` and F12 from `script: lab.ts` to it, a breakpoint in `lab.ts` hit from the Test Explorer's **Debug**,
**Ductus: debug these tests** with no `launch.json`, **Deploy to dev** against a real `ductus
dev`, by the command and on save, the **History** of an interface from that `ductus dev` (the two
deploys there) and from the folder, a message that `ductus dev` stored opened through the web UI's
**Debug in VS Code** link and stopped at a breakpoint, and **Promote…** across two engines in release mode (test, and
prod requiring test first): a release file refused by prod's rules, deployed to test, then
promoted from test to prod, each token asked for once and a viewer's turned away before anything
ran.
