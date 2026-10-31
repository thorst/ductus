# Nuances: TLS on what the engine connects to

What could surprise you here, why it is so, and what to do. Each entry has a level, 1 (day one) to 5 (expert): the [index](index.md) lists them all by level.

## A private CA is trusted only where you name it (level 2)
- **What:** every connection the engine makes over TLS (MLLP destinations with `tls:`, HTTPS destinations and `apis:`, the directory, Postgres `databases:` entries, the alerts' mail server, webhooks and heartbeat) trusts what this server's system trusts, plus the CA its `ca:` names (a `certificates:` entry in resources.yaml), for that connection only. `tls: true` on an MLLP destination is the system's alone. The receiver's certificate must also cover the `host` (or URL host) connected to: a certificate for `hie.example.org` is refused at `10.1.2.3`, unless it lists that address.
- **Why:** a hospital CA added to the whole server would be trusted by every connection, and every program on it; naming it per connection keeps a partner's CA to that partner.
- **What to do:** ask the receiver's team for the CA that issued their certificate (not their certificate itself, which changes at every renewal), put it under `certificates:` (`ca_file:` or `ca: ${secret:...}`), and name it in `ca:`. Connect by the name on their certificate. `ductus validate` and the engine's start refuse a `ca:` that names a pair, a `certificate:` that names a CA, or a name that isn't there.

## A refused handshake is a temporary error: the message waits (level 2)
- **What:** when TLS fails (the receiver's certificate is from a CA we don't trust, doesn't cover the host, has expired; the receiver wants a client certificate we didn't present, or refuses ours), the delivery is a temporary failure: it retries with the usual back-off, nothing is quarantined, and the destination's error says why in words ("TLS: the receiver wants a client certificate (name one in `tls.certificate`)"). With TLS 1.3 the receiver judges our client certificate after our side has finished the handshake, so the refusal arrives as the answer to the first message: that message was not delivered, and is sent again.
- **What to do:** fix the configuration (or ask the receiver to); the waiting messages go as soon as the handshake works, in order. `tls:` pointed at a port that speaks plain MLLP gets no handshake and times out ("timed out waiting for ACK"): check with the receiver which port is TLS.

## Our client certificate: tell the receiver what to expect (level 2)
- **What:** `certificate:` (MLLP: `tls: { certificate: ... }`) presents a `certificates:` pair as our client certificate (mutual TLS). The receiver checks it against the CA it trusts for clients and often against its subject. A pair from files renews like ours (replace both files; new connections present the new one within a minute, no restart), as a listener's does; the mail server's connection picks it up at the next mail.
- **What to do:** give the receiver's team the certificate's subject and the CA that issued it, and renew it before it expires (the receiver refuses an expired one, and every message waits). One pair can serve as both a listener's certificate and a client certificate if its CA allows both uses.

## An HTTP destination through an API uses the API's TLS (level 2)
- **What:** `ca:` and `certificate:` go on the `apis:` entry, which `api()` calls and `http: { api: ... }` destinations share. On an HTTP destination they are only for one with its own `url:` (https://): `validate` refuses them next to `api:` or on an http:// URL.
- **What to do:** put TLS where the URL is.

## Doctor makes the engine's handshake (level 3)
- **What:** `ductus doctor`, the scheduled checks and the Connections page build each TLS connection from the same settings the engine uses (the named CA, the client certificate), so a check passes when the engine's connection would. After a TLS 1.3 handshake it waits up to half a second for the server to refuse the client certificate (TLS 1.3 says so only then; TLS 1.2 says so inside the handshake, so there's no wait), and says which. A certificate named in `ca:` or `certificate:` that doesn't load fails the checks that use it, with why, instead of stopping the run.
- **What to do:** after changing TLS settings, run `ductus doctor --interface <name>` before deploying; the MLLP check connects and shakes hands but sends nothing.
