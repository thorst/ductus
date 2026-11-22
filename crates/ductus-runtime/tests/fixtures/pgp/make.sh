#!/bin/bash
# The OpenPGP test fixtures (pgp.rs tests, demo-pgp.sh): made with GnuPG in a throwaway keyring,
# committed so tests don't need GnuPG. Test keys only: nothing here protects anything. Run it again
# to make new ones (every fixture changes together).
set -euo pipefail
command -v gpg >/dev/null || { echo "no gpg"; exit 1; }
gpg --version | head -1
OUT=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
export GNUPGHOME=$(mktemp -d)
trap 'gpgconf --kill all; rm -rf "$GNUPGHOME"' EXIT
Q="--batch --yes --pinentry-mode loopback"

# Ours: ed25519 primary (sign, certify) + cv25519 encryption subkey, passphrase "ductus-test", no expiry.
gpg $Q --passphrase ductus-test --quick-gen-key "Ductus Test <ductus@example.org>" ed25519 sign,cert never
OURS=$(gpg --list-keys --with-colons ductus@example.org | awk -F: '/^fpr/{print $10; exit}')
gpg $Q --passphrase ductus-test --quick-add-key "$OURS" cv25519 encr never

# The partner: RSA 3072 (what many payers' tools make), no passphrase, encryption subkey.
gpg $Q --passphrase '' --quick-gen-key "Payer EDI <edi@payer.example>" rsa3072 sign,cert never
PAYER=$(gpg --list-keys --with-colons edi@payer.example | awk -F: '/^fpr/{print $10; exit}')
gpg $Q --passphrase '' --quick-add-key "$PAYER" rsa3072 encr never

# A key whose encryption subkey ends 2099-01-01 (primary never): its expiry is the subkey's.
gpg $Q --passphrase '' --quick-gen-key "Dated <dated@example.org>" ed25519 sign,cert never
DATED=$(gpg --list-keys --with-colons dated@example.org | awk -F: '/^fpr/{print $10; exit}')
gpg $Q --passphrase '' --quick-add-key "$DATED" cv25519 encr 2099-01-01

# A key made in 2019 that expired in 2021.
gpg $Q --passphrase '' --faked-system-time 20190101T000000 --quick-gen-key "Expired <expired@example.org>" ed25519 sign,cert 2021-01-01
EXPIRED=$(gpg --list-keys --with-colons expired@example.org | awk -F: '/^fpr/{print $10; exit}')
gpg $Q --passphrase '' --faked-system-time 20190101T000000 --quick-add-key "$EXPIRED" cv25519 encr 2021-01-01

gpg $Q --passphrase ductus-test --armor --export-secret-keys "$OURS" >"$OUT/ours.sec.asc"
gpg --armor --export "$OURS" >"$OUT/ours.pub.asc"
gpg $Q --passphrase '' --armor --export-secret-keys "$PAYER" >"$OUT/payer.sec.asc"
gpg --armor --export "$PAYER" >"$OUT/payer.pub.asc"
gpg --export "$PAYER" >"$OUT/payer.pub.gpg"
gpg --armor --export "$DATED" >"$OUT/dated.pub.asc"
gpg --armor --export "$EXPIRED" >"$OUT/expired.pub.asc"

# What the payer sends us: an HL7 batch of 3, encrypted to us and signed by them (as `gpg -es`
# makes it: compressed inside), binary and armored; one signed by nobody; one to the wrong key.
printf 'MSH|^~\\&|PAYER|P|DUCTUS|D|20261121||ADT^A01|1|P|2.5\rPID|1||100\rMSH|^~\\&|PAYER|P|DUCTUS|D|20261121||ADT^A01|2|P|2.5\rPID|1||200\rMSH|^~\\&|PAYER|P|DUCTUS|D|20261121||ADT^A01|3|P|2.5\rPID|1||300\r' >"$GNUPGHOME/batch.hl7"
cp "$GNUPGHOME/batch.hl7" "$OUT/batch.hl7"
gpg $Q --passphrase '' --trust-model always -u "$PAYER" -r "$OURS" --sign --encrypt -o "$OUT/from-payer.hl7.pgp" "$GNUPGHOME/batch.hl7"
gpg $Q --passphrase '' --trust-model always -u "$PAYER" -r "$OURS" --sign --encrypt --armor -o "$OUT/from-payer.hl7.asc" "$GNUPGHOME/batch.hl7"
gpg $Q --trust-model always -r "$OURS" --encrypt -o "$OUT/unsigned.hl7.pgp" "$GNUPGHOME/batch.hl7"
gpg $Q --trust-model always -r "$PAYER" --encrypt -o "$OUT/not-for-us.hl7.pgp" "$GNUPGHOME/batch.hl7"
ls -l "$OUT"
