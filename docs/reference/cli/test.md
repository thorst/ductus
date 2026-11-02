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
| `--interface <INTERFACE>` | Only this interface. |  |  |
| `--filter <FILTER>` | Only the tests whose name contains this text (case matters): `--filter "A31"`. |  |  |
| `--list` | List the tests, with the file and line of each, without running them. |  |  |
| `--json` | JSON Lines for tools (an editor, CI): one object per test (its interface, name, file, line, column, and when run `ok`, `ms`, the error, where it failed and the expected and actual values), then a summary. The testing guide describes every field. |  |  |
| `--env <ENV>` | The environment: `${NAME}` values come from environments/<env>.yaml, `${secret:NAME}` from secrets/<env>.json. | `dev` | `DUCTUS_ENV` |

