# OpenPGP: encrypted and signed files

Payers, banks and clearinghouses often won't take a file in the clear, even over SFTP: they ask
for it **encrypted** to their OpenPGP key (only they can read it) and **signed** with yours (they
can tell it came from you), and they send theirs back the same way. File and SFTP sources and
destinations do both, with `pgp:`, and the keys are named once in resources.yaml, like
certificates.

| Where | Setting | The engine |
|---|---|---|
| A `file` or `sftp` **source** | `pgp: { decrypt: ours, verify: payer }` | Decrypts each file with our key, takes it only if the partner signed it, then reads it as usual (`split: records` too) |
| A `file` or `sftp` **destination** | `pgp: { encrypt: payer, sign: ours }` | Signs each file with our key, encrypts it to theirs, names it `1234.hl7.pgp` |

**When not:** for a connection that's already TLS end to end (HTTPS, MLLP over TLS) to a partner
who doesn't ask for it, `pgp:` adds nothing. It's for files: what sits in a folder or on an SFTP
server between the two of you.

## Keys in resources.yaml

```yaml
# resources.yaml
pgp:
  ours:                                     # our key: decrypts what partners send, signs what we send
    key_file: /etc/ductus/pgp/hospital.asc # our secret key, armored or binary
    passphrase: ${secret:PGP_PASSPHRASE}    # if it has one (GnuPG asks for one by default)
    owner: edi-team@hospital.org            # who replaces it: expiry alerts reach them
  payer:                                    # a partner's public key
    public_key_file: /etc/ductus/pgp/payer.asc
    vendor: Availity
```

- **Ours** is a secret key (`key_file`, or `key` as text from a secret). Make it once with GnuPG
  (`gpg --quick-gen-key "Hospital EDI <edi@hospital.org>"`, then `gpg --export-secret-keys
  --armor edi@hospital.org > hospital.asc`) and give each partner its public half (`gpg --export
  --armor edi@hospital.org`). Keep the file readable by the engine's account only (`chmod 600`),
  outside the project (`/etc/ductus/pgp`, or a path from the environment), as for
  [certificate keys](tls.md#certificates-in-resourcesyaml). Its `passphrase` unlocks it once, as
  it loads; the unlocked key stays in the engine's memory
  ([nuance](../nuances/pgp.md#our-keys-passphrase-is-used-once-as-it-loads-level-3)).
- **A partner's** is their public key (`public_key_file`, or `public_key` as text), as they sent
  it, armored or binary.
- **Checked before anything runs**: every key loads at start, before the database: one that
  can't be read, is in the wrong slot (a public key in `key_file`), has a wrong or missing
  passphrase, was revoked or has expired stops the start, saying which and why. `ductus validate`
  checks the same, with each key's user id, fingerprint and expiry.
- **Replaced without a restart**: key files are read again every minute, like certificate files.
  A new file that can't be used keeps the key loaded before, with why on the Doctor page.
- **Expiry is watched** like the certificates': the Doctor page's "Ours" lists every key
  interfaces use, what for, and its days left, and a rule naming `certificate` alerts at 30, 14
  and 7 days ([nuance](../nuances/pgp.md#keys-count-down-like-certificates-to-their-last-usable-part-level-2)).

## Picking up encrypted files

```yaml
# interfaces/claims-in/interface.yaml, in part
source:
  sftp:
    server: payer_sftp
    dir: outbound
    pattern: "*.pgp"
    split: records
    pgp: { decrypt: ours, verify: payer }
format: hl7
```

- **`decrypt:`** takes only files encrypted to our key; **`verify:`** takes only files signed by
  the partner's ([nuance](../nuances/pgp.md#verify-takes-only-signed-files-level-2)). Give one or
  both. Armored (`.asc`) and binary (`.pgp`, `.gpg`) files both work, compressed or not, as GnuPG
  and partners' tools make them.
- **Nothing in a file is stored until its signature is checked**: it's decrypted into a
  temporary file only the engine's account can read
  ([nuance](../nuances/pgp.md#decrypted-text-is-in-a-temporary-file-while-a-file-is-read-level-3)),
  checked to its end, then read like any file. `split: records` splits the decrypted text, and
  after a stop carries on from the last record stored
  ([nuance](../nuances/pgp.md#a-resumed-file-is-decrypted-again-level-4)).
- **A file that won't open is quarantined, and the folder goes on**: one not encrypted to our
  key, not signed by theirs, sent in the clear, or damaged on the way is stored once, as received,
  quarantined with why (the Messages page shows it), moved to the archive like any file, and the
  next file is read. Resending it can't help
  ([nuance](../nuances/pgp.md#a-file-that-wont-open-is-quarantined-and-the-next-file-is-read-level-2)).
  Temporary failures (the SFTP server down) still stop and retry, as for any file.
- **`ductus try` takes the file as the partner sent it**: given an encrypted file, it decrypts
  and checks it with the project's keys first, as the engine would, or says why it would be
  quarantined.

## Sending encrypted files

```yaml
# interfaces/claims-out/interface.yaml, in part
destinations:
  payer:
    sftp: { server: payer_sftp, dir: inbound, pgp: { encrypt: payer, sign: ours } }
```

- Each message is **signed with our key, then encrypted to theirs**, and written as
  `<id>.<format>.pgp` (`1234.x12.pgp`); `armor: true` writes text (`-----BEGIN PGP MESSAGE-----`)
  named `.asc`, for partners whose tools ask for it. Give `encrypt:`, `sign:` or both.
- What partners get is what GnuPG makes by default (OpenPGP SEIPD with AES-256), so any tool
  reads it ([nuance](../nuances/pgp.md#what-partners-get-level-3)).
- **Cost:** sealing a message takes well under a millisecond, and opening a file runs at over
  100 MB/s: encrypted files are stored about as fast as plain ones
  ([measured](../nuances/pgp.md#what-openpgp-costs-level-4)).
- **A key that can't be used leaves messages waiting, never sent in the clear**: they're retried,
  with why on the destination, until the key is replaced
  ([nuance](../nuances/pgp.md#a-destination-whose-key-cant-be-used-waits-level-3)).

## What `validate` checks

`decrypt:` and `sign:` need our secret key (a `pgp:` entry with `key_file` or `key`): naming a
partner's public key there is DUC214. A key resources.yaml doesn't have is DUC202; one that
doesn't load DUC215; one expiring within 30 days DUC216 (a warning).
