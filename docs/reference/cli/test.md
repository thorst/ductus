# `ductus test`

Run each interface's `*.test.ts` files.

Each test runs inside the engine's own JavaScript runtime with the SDK it uses, one at a time, with no database, network or engine. A failed test prints where it is, where it failed (the `expect`'s line, or the stack of what was thrown) and why; exits 1 when one fails, or when `--filter` matches no test.

```text
ductus test [OPTIONS]
```

**Options**

| | What it does | Default | Environment |
|---|---|---|---|
| `--root <ROOT>` | The project folder: the one with `interfaces/` and ductus.yaml. | `.` |  |
| `--interface <INTERFACE>` | Only this interface: its name, or a file or folder inside its folder (`interfaces/adt-in/route.test.ts`), as an editor passes the open file. |  |  |
| `--filter <FILTER>` | Only the tests whose name contains this text (case matters): `--filter "A31"`. |  |  |
| `--list` | List the tests, with the file and line of each, without running them. |  |  |
| `--inspect <ADDRESS>` | Run the tests under a debugger (VS Code, Chrome DevTools, anything that speaks the Chrome DevTools Protocol): listen on 127.0.0.1:9229 (or the address given, on this machine only), wait for one to attach, then run one interface's tests (`--interface`, or the only one with tests). Breakpoints and `debugger;` statements pause there; the time limit is off while it's attached. The debugging guide shows VS Code's and Chrome's setup. |  |  |
| `--json` | JSON Lines for tools (an editor, CI): one object per test (its interface, name, file, line, column, and when run `ok`, `ms`, the error, where it failed and the expected and actual values), then a summary. The testing guide describes every field. |  |  |
| `--env <ENV>` | The environment: `${NAME}` values come from environments/<env>.yaml, `${secret:NAME}` from secrets/<env>.json. | `dev` | `DUCTUS_ENV` |

