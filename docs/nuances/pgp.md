# Nuances: OpenPGP on files

What could surprise you here, why it is so, and what to do. Each entry has a level, 1 (day one) to 5 (expert): the [index](index.md) lists them all by level.

## A file that won't open is quarantined, and the next file is read (level 2)
- **What:** with `pgp:` on a source, a file that isn't encrypted to our key, isn't signed by the partner's (with `verify:`), was sent in the clear (with `decrypt:`) or was damaged is stored once as received, quarantined at every destination with why (it starts "OpenPGP:"), then moved to the archive (or deleted, with `after: delete`) like any file, and the folder goes on. Failures that may pass (the SFTP server down, this server's disk full) stop at that file and retry, as for any file. One over 256 MB that won't open is left in place with the reason in the log (too big to keep as a message).
- **Why:** a file that won't decrypt today won't tomorrow: the fix is a key, not a retry. Stopping the folder at it would hold every file after it, from the same partner, all day.
- **What to do:** read the reason on the message. "isn't encrypted to our key ... it's for key 0x…" means the partner used another key of ours (an old one: send them the current public key); "signature isn't by" means they sign with a key we don't have (ask for their new public key). Fix the key in resources.yaml, then put the archived file back in the folder: it's read as new. Resending the quarantined message can't help (it's the file as received).

## `verify:` takes only signed files (level 2)
- **What:** with `verify: payer`, a file with no signature is quarantined, as is one signed by any other key or whose signature doesn't match its contents. Without `verify:`, signatures aren't checked (a signed file is read all the same).
- **Why:** `verify:` is there to know who sent a file; an unsigned file can't say.
- **What to do:** set `verify:` for partners who sign, and leave it out for those who don't.

## Keys count down like certificates, to their last usable part (level 2)
- **What:** every key an interface uses is on the Doctor page's "Ours", with what it's used for and its days left, and alerts with a rule naming `certificate` at 30, 14 and 7 days, then as critical once expired. An OpenPGP key has parts (its primary key, an encryption subkey, sometimes a signing subkey), each with its own expiry: the countdown runs to when the key can no longer do what it does now (an older encryption subkey ending while a newer one goes on ends nothing). A key that never expires shows "Never expires". An expired key stops the start (as an expired certificate does).
- **What to do:** for our key, extend it before then (`gpg --quick-set-expire <fingerprint> 2y`, and for its subkeys `gpg --quick-set-expire <fingerprint> 2y '*'`), export it again over the file, and send partners the new public key (an extended key is the same key: they import it again). For a partner's, ask them for their new public key and replace the file. Both are read within a minute.

## Decrypted text is in a temporary file while a file is read (level 3)
- **What:** each file is decrypted into a file in the system's temporary folder (`ductus-pgp-…`, readable by the engine's account only), checked to its end, read, and deleted once its messages are stored. A file from SFTP is copied there first too. So a file needs about its own size free there twice over, while it's read.
- **Why:** nothing of a file is stored until its signature, at its end, is checked, and `split: records` resumes by offsets into the decrypted text: both need it somewhere other than memory.
- **What to do:** on Linux `/tmp` may be in memory (tmpfs): for big files, point the service at a folder on disk (`sudo systemctl edit ductus`, `[Service]`, `Environment=TMPDIR=/var/lib/ductus/tmp`, and create the folder for the `ductus` user). On Windows it's the service account's own temporary folder.

## Our key's passphrase is used once, as it loads (level 3)
- **What:** the passphrase unlocks our secret key when it's read (at start, and when its file changes); the unlocked key stays in the engine's memory, so no file pays for the key derivation (GnuPG's default takes tens of milliseconds, argon2 more). A wrong passphrase stops the start, saying so.
- **What to do:** keep the passphrase in a secret (`${secret:PGP_PASSPHRASE}`) and the key file readable by the engine's account only.

## A destination whose key can't be used waits (level 3)
- **What:** with `pgp:` on a destination, a message whose file can't be sealed (the partner's key has no part that may still encrypt, ours none that may sign) isn't written: it's retried, with why on the destination, and the messages behind it in its lane wait too. It's never written unencrypted.
- **What to do:** replace the key (its file is read within a minute); the waiting messages then go.

## What partners get (level 3)
- **What:** each file is signed with our key (SHA-256) and encrypted to the newest part of the partner's key that may encrypt, as OpenPGP "SEIPD v1" with AES-256, uncompressed: what GnuPG 2.2 and later, Kleopatra, and partners' EDI tools read. Binary by default (`.pgp`), armored text with `armor: true` (`.asc`). Files partners send may be any of what GnuPG makes, AEAD (GnuPG 2.4's OCB) and compressed ones included; old files without an integrity check (pre-2000 tools) are refused as damaged.

## What OpenPGP costs (level 4)
- **What:** measured 2026-11-21 (WSL2 on a desktop, release build): opening a 100 MB signed and encrypted file (decrypt, check the signature, write the text out) took 0.89 s, 118 MB/s; a payer's day of claims files is seconds. End to end, a 200,000-row encrypted CSV split into records was stored at 8,844 rows/s against 9,222 for the same file in the clear (4% less: storing, not decrypting, is the limit). Sealing one HL7 message to send (sign with Ed25519 + encrypt) took 143 µs to a Curve25519 key and 541 µs to an RSA 3072 one, done by the destination's worker before it writes: next to an SFTP upload's tens of milliseconds, nothing.
- **What to do:** nothing for files partners exchange. Only a destination writing well over a thousand files a second to an RSA key would find sealing its limit (each costs about half a millisecond of one core).

## A resumed file is decrypted again (level 4)
- **What:** after a stop partway through an encrypted file with `split: records`, the file is decrypted again from its start and reading carries on from the last record stored: no record is lost or stored twice, but the decryption is paid again.
- **Why:** encrypted data can't be entered in the middle.
