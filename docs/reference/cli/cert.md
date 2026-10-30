# `ductus cert`

Certificates for `tls:` on listeners (`certificates:` in resources.yaml).

```text
ductus cert [OPTIONS] <COMMAND>
```

## `ductus cert request`

A new key and a certificate signing request (CSR) for the hospital's CA: writes OUT.key (private, never overwritten) and OUT.csr, and says what to send, what comes back and where it goes.

```text
ductus cert request [OPTIONS] <OUT>
```

**Arguments**

| | What it does | Default | Environment |
|---|---|---|---|
| `<OUT>` | Where to write, without the extension: `tls/mllp` writes tls/mllp.key and tls/mllp.csr. |  |  |

**Options**

| | What it does | Default | Environment |
|---|---|---|---|
| `--dns <DNS>` | A DNS name senders connect to (repeat for several; the first is the CN). |  |  |
| `--ip <IPS>` | An IP address senders connect to (repeat for several). |  |  |
| `--subject <SUBJECT>` | The rest of the subject, as the CA wants it: "O=General Hospital, OU=Interfaces, C=US". |  |  |
| `--key <KEY>` | The key: rsa (2048, what every CA takes) or ecdsa (P-256). | `rsa` |  |

## `ductus cert show`

What a certificate file or a `certificates:` entry holds: subject, names, issuer, the chain's order and completeness, days left, and whether the key is the certificate's. Exits 1 when the engine or senders would refuse it.

```text
ductus cert show [OPTIONS] <TARGET>
```

**Arguments**

| | What it does | Default | Environment |
|---|---|---|---|
| `<TARGET>` | A PEM file, or the name of a `certificates:` entry in resources.yaml. |  |  |

**Options**

| | What it does | Default | Environment |
|---|---|---|---|
| `--key <KEY>` | The key to check (default: the .key file beside a .pem, or the entry's key). |  |  |
| `--root <ROOT>` | The project folder: the one with `interfaces/` and ductus.yaml. | `.` |  |
| `--env <ENV>` | The environment: `${NAME}` values come from environments/<env>.yaml, `${secret:NAME}` from secrets/<env>.json. | `dev` | `DUCTUS_ENV` |

## `ductus cert self-signed`

A self-signed certificate and its key, for a lab or a test: writes OUT.pem and OUT.key and prints the resources.yaml entry. Senders must be given OUT.pem to trust. Production certificates come from the hospital's CA.

```text
ductus cert self-signed [OPTIONS] --name <NAMES> <OUT>
```

**Arguments**

| | What it does | Default | Environment |
|---|---|---|---|
| `<OUT>` | Where to write, without the extension: `tls/engine` writes tls/engine.pem and tls/engine.key. |  |  |

**Options**

| | What it does | Default | Environment |
|---|---|---|---|
| `--name <NAMES>` | A DNS name or IP address senders connect to (repeat for several). |  |  |
| `--days <DAYS>` | How long it is valid. | `365` |  |

