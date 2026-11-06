# `ductus try`

Run one message through an interface's code, every destination, and show what each would do with it now: sent (and what), filtered and why, quarantined and why. Sends nothing and writes nothing: lookups (`db().query`, GET requests) are made, writes (`db().execute`, other requests) are refused and reported. The message comes from a file, a fixture, or the store (`--message`), then with what each destination did with it then, and, when other code decided that, which code version did (the first line names the version tried). With `--inspect`, under a debugger, as `ductus test --inspect`.

```text
ductus try [OPTIONS] <INTERFACE> [FILE]
```

**Arguments**

| | What it does | Default | Environment |
|---|---|---|---|
| `<INTERFACE>` | The interface: its name, or a file or folder inside its folder. |  |  |
| `<FILE>` | A file holding the message as the sender sends it. |  |  |

**Options**

| | What it does | Default | Environment |
|---|---|---|---|
| `--fixture <FIXTURE>` | A file in the interface's `fixtures/` folder instead. |  |  |
| `--message <MESSAGE>` | A stored message, by id: read from the database (DUCTUS_DATABASE_URL), or from the running engine at `--from` through its API. |  |  |
| `--from <FROM>` | With `--message`: the engine's REST API to read it from (`https://ductus-prod:7700`): a token with the `payloads` permission (DUCTUS_API_TOKEN), and the read is in that engine's audit log. |  |  |
| `--destination <DESTINATION>` | Only this destination's code (by its name in interface.yaml), when the others don't matter or aren't ready: the rest aren't run. |  |  |
| `--token <TOKEN>` | The token for `--from`. |  | `DUCTUS_API_TOKEN` |
| `--database-url <DATABASE_URL>` | With `--message` and no `--from`: the database. |  | `DUCTUS_DATABASE_URL` |
| `--root <ROOT>` | The project folder: the one with `interfaces/` and ductus.yaml. | `.` |  |
| `--env <ENV>` | The environment whose values and resources the code uses (lookups go to its databases). | `dev` | `DUCTUS_ENV` |
| `--inspect <ADDRESS>` | Under a debugger, as `ductus test --inspect`: listen on 127.0.0.1:9229 (or the address given, on this machine only) and wait for one before the code loads. |  |  |
| `--json` | One JSON document: each destination's decision, output, console lines and what it recorded then. |  |  |

