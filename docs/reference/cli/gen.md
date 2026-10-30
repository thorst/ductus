# `ductus gen`

Send synthetic ADT messages over MLLP.

```text
ductus gen [OPTIONS]
```

**Options**

| | What it does | Default | Environment |
|---|---|---|---|
| `--to <TO>` | The MLLP listener to send to, `host:port`. | `127.0.0.1:6661` |  |
| `--count <COUNT>` | How many messages to send. | `1000` |  |
| `--patients <PATIENTS>` | Distinct patients (MRNs) to spread messages across. | `200` |  |
| `--rate <RATE>` | Total messages per second; 0 = as fast as ACKs come back. | `0` |  |
| `--connections <CONNECTIONS>` | Concurrent MLLP connections. Each patient always uses the same connection. | `1` |  |
| `--hot <HOT>` | Fraction of messages (0..1) for one hot patient, e.g. 0.3. | `0` |  |
| `--offset <OFFSET>` | Added to every sequence number and patient number, so several generators can feed one sink without their messages looking like duplicates. | `0` |  |
| `--tls` | Send over TLS (also on with --ca or --cert), checking the receiver's certificate against this system's trusted roots and --ca, as the engine's `tls:` does. |  |  |
| `--ca <CA>` | A CA (PEM) the receiver's certificate may come from (a self-signed one: that file). |  |  |
| `--cert <CERT>` | Present this client certificate (PEM, ours first), with --key. |  |  |
| `--key <KEY>` | The client certificate's private key (PEM). |  |  |
| `--from <FROM>` | Connect from this address of this machine (127.0.0.2 on Linux, say), to try a receiver's `allow:` list as a sender elsewhere would. |  |  |

