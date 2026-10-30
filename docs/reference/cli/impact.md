# `ductus impact`

Which interfaces a vendor, application or host touches, how, and who owns them: for a vendor breach or a VPN/IP change. Reads `about:`, destinations, resources.yaml, and the databases and APIs code uses.

```text
ductus impact [OPTIONS] <--vendor <VENDOR>|--application <APPLICATION>|--host <HOST>>
```

**Options**

| | What it does | Default | Environment |
|---|---|---|---|
| `--vendor <VENDOR>` | e.g. "Epic Systems" (ignores case). |  |  |
| `--application <APPLICATION>` | e.g. "Sunquest Lab" (ignores case). |  |  |
| `--host <HOST>` | An address, subnet (10.20.30.0/24) or host name. |  |  |
| `--resolve` | Look host names up in DNS, so names in the config match addresses (and the other way round). |  |  |
| `--root <ROOT>` | The project folder: the one with `interfaces/` and ductus.yaml. | `.` |  |
| `--env <ENV>` | The environment: `${NAME}` values come from environments/<env>.yaml, `${secret:NAME}` from secrets/<env>.json. | `dev` | `DUCTUS_ENV` |

