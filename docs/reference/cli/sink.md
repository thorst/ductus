# `ductus sink`

Receive messages over MLLP, ACK them, and verify per-patient ordering. With --smtp or --http, a mail server or web server that accepts everything and prints it instead (to see what alerts send without a real one).

```text
ductus sink [OPTIONS]
```

**Options**

| | What it does | Default | Environment |
|---|---|---|---|
| `--port <PORT>` | The MLLP port to listen on (127.0.0.1). | `6662` |  |
| `--smtp <SMTP>` | Be an SMTP server on this port: print each mail (and keep it in --dir). |  |  |
| `--http <HTTP>` | Be an HTTP server on this port: print each request's body (and keep it in --dir). |  |  |
| `--dir <DIR>` | Also write each mail or request to a file of its own here. |  |  |
| `--reject <REJECT>` | Answer AR (application reject) to messages that contain this text, as a receiver that refuses them would (repeat for several). The engine quarantines them. |  |  |
| `--tls` | Take MLLP over TLS only, presenting --cert and --key (as the engine's `tls:` on a listener). |  |  |
| `--cert <CERT>` | The certificate to present (PEM, ours first, then intermediates). |  |  |
| `--key <KEY>` | The certificate's private key (PEM). |  |  |
| `--clients-ca <CLIENTS_CA>` | Ask senders for a client certificate this CA (PEM) issued; others are refused. |  |  |
| `--subject <SUBJECTS>` | Only client certificates whose subject matches (`CN=bridges*.hospital.org`; repeat). |  |  |

