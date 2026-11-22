//! OpenPGP for files (0.7.8.1), with rPGP (the `pgp` crate: pure Rust, so the same on Linux and
//! Windows, nothing to install).
//!
//! - **Keys** (`pgp:` in resources.yaml) load at start with the certificates
//!   (`tls::Certificates`), each checked as it loads: its self-signatures, its expiry, a revoked
//!   key refused, our secret key unlocked with its passphrase once (in memory: no key derivation
//!   for each file). Key files are re-read every minute with the certificate files, so a new key
//!   is used without a restart; their expiry is in the certificate table and its alerts.
//! - **Opening** a file a partner sent (`open`): decrypted and its signature checked as one
//!   stream, written out as it goes; nothing of it is stored until the signature is checked at
//!   its end. A file that isn't what `pgp:` asks for is `Refused` with why (it's quarantined; the
//!   folder goes on); a failure to write the text out is an error (tried again).
//! - **Sealing** a file we send (`seal`): signed, then encrypted (SEIPD v1 with AES-256, what
//!   GnuPG 2.2 and later and every partner tool read), binary or armored.

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use pgp::composed::{
    ArmorOptions, DecryptionOptions, Deserializable, Esk, Message, MessageBuilder, SignedPublicKey, SignedSecretKey,
    TheRing, VerificationResult,
};
use pgp::crypto::hash::HashAlgorithm;
use pgp::crypto::sym::SymmetricKeyAlgorithm;
use pgp::packet::{KeyFlags, Signature, SignatureType};
use pgp::types::{KeyDetails, Password, SecretParams, Seipdv1ReadMode, SigningKey, Timestamp, VerifyingKey};
use sha2::{Digest, Sha256};
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

/// What a key is, as the certificate table and the Doctor page show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct About {
    /// Its primary user id (`Payer EDI <edi@payer.example>`), or its key id when it has none.
    pub user_id: String,
    /// The primary key's fingerprint, upper-case hex: what `gpg --fingerprint` shows, without spaces.
    pub fingerprint: String,
    pub created: DateTime<Utc>,
    /// When it can no longer do all it does now (its primary key's expiry, or the last of its
    /// encryption or signing subkeys'): None if nothing in it expires.
    pub not_after: Option<DateTime<Utc>>,
    /// What ends first, when it isn't the primary key: `its encryption subkey`.
    pub earliest: Option<String>,
    /// Ours (a secret key), or a partner's public key.
    pub secret: bool,
}

impl About {
    /// `RSA key 0x1234ABCD… (Payer EDI <edi@payer.example>)`: the key as messages name it.
    pub fn named(&self) -> String {
        format!("{} ({})", short(&self.fingerprint), self.user_id)
    }
}

/// `0x` and the fingerprint's last 16 hex digits: the "long key id" GnuPG lists.
fn short(fingerprint: &str) -> String {
    format!("0x{}", &fingerprint[fingerprint.len().saturating_sub(16)..])
}

/// One component (the primary key or a subkey) and what it may do.
struct Component {
    what: &'static str,
    created: DateTime<Utc>,
    expires: Option<DateTime<Utc>>,
    encrypt: bool,
    sign: bool,
}

/// A key as loaded.
struct Loaded {
    public: SignedPublicKey,
    /// Ours: unlocked (its passphrase removed, in memory only).
    secret: Option<SignedSecretKey>,
    about: About,
    /// Of the text it was read from: a file whose contents hash the same hasn't changed.
    hash: [u8; 32],
}

/// A `pgp:` entry, loaded; re-read from its file when it changes.
pub struct Key {
    pub name: String,
    file: Option<PathBuf>,
    passphrase: Option<String>,
    current: RwLock<Arc<Loaded>>,
    /// The last change that didn't load, and why (logged once, not every minute).
    failed: Mutex<Option<([u8; 32], String)>>,
}

// Hand-written so key material and passphrases never end up in logs.
impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Key").field("name", &self.name).field("file", &self.file).finish_non_exhaustive()
    }
}

fn to_time(t: Timestamp) -> DateTime<Utc> {
    DateTime::from_timestamp(i64::from(t.as_secs()), 0).unwrap_or_default()
}

/// The newest of `signatures` of a kind (by creation time).
fn newest<'a>(signatures: impl Iterator<Item = &'a Signature>) -> Option<&'a Signature> {
    signatures.max_by_key(|s| s.created().map(Timestamp::as_secs).unwrap_or(0))
}

/// When a component made at `created` expires, by its newest self-signature.
fn expiry(created: DateTime<Utc>, signature: Option<&Signature>) -> Option<DateTime<Utc>> {
    let seconds = std::time::Duration::from(signature?.key_expiration_time()?).as_secs();
    (seconds > 0).then(|| created + chrono::Duration::seconds(seconds as i64))
}

/// What a component may do: its flags, or (a key from before flags) what its algorithm can.
fn abilities(flags: KeyFlags, algorithm: pgp::crypto::public_key::PublicKeyAlgorithm) -> (bool, bool) {
    if flags == KeyFlags::default() {
        return (algorithm.can_encrypt(), algorithm.can_sign());
    }
    (flags.encrypt_comms() || flags.encrypt_storage(), flags.sign())
}

/// The primary key and each subkey still in force (not revoked), with what each may do.
fn components(key: &SignedPublicKey) -> Result<Vec<Component>> {
    if !key.details.revocation_signatures.is_empty() {
        bail!("it was revoked by its owner: ask them for their new key");
    }
    let primary_created = to_time(key.primary_key.created_at());
    let self_signed = key.details.users.iter().flat_map(|u| u.signatures.iter()).chain(&key.details.direct_signatures);
    let signature = newest(self_signed);
    let (encrypt, sign) = abilities(signature.map(Signature::key_flags).unwrap_or_default(), key.algorithm());
    let mut out = vec![Component {
        what: "its primary key",
        created: primary_created,
        expires: expiry(primary_created, signature),
        encrypt,
        sign,
    }];
    for sub in &key.public_subkeys {
        let Some(binding) = newest(sub.signatures.iter()) else { continue };
        if binding.typ() == Some(SignatureType::SubkeyRevocation) {
            continue;
        }
        let created = to_time(sub.key.created_at());
        let (encrypt, sign) = abilities(binding.key_flags(), sub.key.algorithm());
        let what = match (encrypt, sign) {
            (true, _) => "its encryption subkey",
            (false, true) => "its signing subkey",
            _ => continue, // authentication or certification only: nothing of ours uses it
        };
        out.push(Component { what, created, expires: expiry(created, Some(binding)), encrypt, sign });
    }
    Ok(out)
}

/// When `parts` can no longer do all they do now, and which part ends that (when it isn't the
/// primary key).
fn until(parts: &[Component]) -> (Option<DateTime<Utc>>, Option<String>) {
    let mut ends: Vec<(DateTime<Utc>, &str)> = Vec::new();
    if let Some(primary) = parts[0].expires {
        ends.push((primary, parts[0].what));
    }
    for able in [|c: &Component| c.encrypt, |c: &Component| c.sign] {
        let capable: Vec<&Component> = parts.iter().filter(|c| able(c)).collect();
        // The last of them: an older subkey ending while a newer one goes on ends nothing.
        if !capable.is_empty() && capable.iter().all(|c| c.expires.is_some()) {
            let last = capable.iter().max_by_key(|c| c.expires).expect("not empty");
            ends.push((last.expires.expect("all expire"), last.what));
        }
    }
    match ends.into_iter().min_by_key(|(at, _)| *at) {
        None => (None, None),
        Some((at, what)) => (Some(at), (what != "its primary key").then(|| what.to_string())),
    }
}

fn hash(parts: &[&[u8]]) -> [u8; 32] {
    let mut h = Sha256::new();
    for p in parts {
        h.update((p.len() as u64).to_le_bytes());
        h.update(p);
    }
    h.finalize().into()
}

/// Read a key: a secret key (ours: unlocked with `passphrase`) or a public key, armored or
/// binary, checked (its self-signatures, revoked, expired).
fn load(text: &[u8], secret: bool, passphrase: Option<&str>) -> Result<Loaded> {
    let (public, secret) = if secret {
        let (mut key, _) = SignedSecretKey::from_reader_single(text).map_err(|e| {
            let public = SignedPublicKey::from_reader_single(text).is_ok();
            if public {
                anyhow!("this is a public key; ours must be a secret key (`gpg --export-secret-keys --armor`). A partner's key goes in `public_key_file`")
            } else {
                anyhow!("not an OpenPGP secret key: {e}")
            }
        })?;
        key.verify_bindings().map_err(|e| anyhow!("its self-signatures don't check out: {e}"))?;
        unlock(&mut key, passphrase)?;
        (key.to_public_key(), Some(key))
    } else {
        let (key, _) = SignedPublicKey::from_reader_single(text).map_err(|e| {
            if SignedSecretKey::from_reader_single(text).is_ok() {
                anyhow!("this is a secret key: a partner's public key goes here (keep our secret keys in `key_file`)")
            } else {
                anyhow!("not an OpenPGP public key: {e}")
            }
        })?;
        key.verify_bindings().map_err(|e| anyhow!("its self-signatures don't check out: {e}"))?;
        (key, None)
    };
    let parts = components(&public)?;
    let (not_after, earliest) = until(&parts);
    let user_id = public
        .details
        .users
        .iter()
        .find(|u| u.is_primary())
        .or(public.details.users.first())
        .map(|u| String::from_utf8_lossy(u.id.id()).into_owned())
        .unwrap_or_else(|| short(&format!("{:X}", public.fingerprint())));
    let about = About {
        user_id,
        fingerprint: format!("{:X}", public.fingerprint()),
        created: parts[0].created,
        not_after,
        earliest,
        secret: secret.is_some(),
    };
    Ok(Loaded { public, secret, about, hash: hash(&[text, passphrase.unwrap_or("").as_bytes()]) })
}

/// Remove the secret key's passphrase in memory, so each file costs no key derivation (GnuPG's
/// takes tens of milliseconds, argon2's more).
fn unlock(key: &mut SignedSecretKey, passphrase: Option<&str>) -> Result<()> {
    let locked = matches!(key.primary_key.secret_params(), SecretParams::Encrypted(_))
        || key.secret_subkeys.iter().any(|s| matches!(s.key.secret_params(), SecretParams::Encrypted(_)));
    if !locked {
        return Ok(());
    }
    let Some(passphrase) = passphrase else {
        bail!("the key has a passphrase: give it in `passphrase:` (from a secret)");
    };
    let password = Password::from(passphrase);
    let wrong = |e| anyhow!("its passphrase didn't unlock it (is `passphrase:` this key's?): {e}");
    key.primary_key.remove_password(&password).map_err(wrong)?;
    for sub in &mut key.secret_subkeys {
        sub.key.remove_password(&password).map_err(wrong)?;
    }
    Ok(())
}

impl Key {
    /// Load the `pgp:` entry `name`: refused if it doesn't load, or has expired.
    pub fn load(name: &str, entry: &ductus_core::pgp::PgpKey) -> Result<Self> {
        use ductus_core::pgp::KeyText;
        let text = entry.text().context("neither our secret key nor a partner's public key")?;
        let bytes = match text {
            KeyText::SecretFile(f) | KeyText::PublicFile(f) => {
                std::fs::read(f).with_context(|| format!("reading {}", f.display()))?
            }
            KeyText::Secret(t) | KeyText::Public(t) => t.as_bytes().to_vec(),
        };
        let loaded = load(&bytes, text.is_secret(), entry.passphrase.as_deref())?;
        if let Some(at) = loaded.about.not_after.filter(|at| *at < Utc::now()) {
            bail!(
                "it expired on {} ({}): ask for a new one, or extend it (`gpg --quick-set-expire`) and export it again",
                at.format("%Y-%m-%d"),
                loaded.about.named()
            );
        }
        Ok(Self {
            name: name.to_string(),
            file: entry.file().map(PathBuf::from),
            passphrase: entry.passphrase.clone(),
            current: RwLock::new(Arc::new(loaded)),
            failed: Mutex::new(None),
        })
    }

    /// Read from a file (re-read every minute), not text from a secret.
    pub fn from_file(&self) -> bool {
        self.file.is_some()
    }

    fn current(&self) -> Arc<Loaded> {
        self.current.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn about(&self) -> About {
        self.current().about.clone()
    }

    /// Why the file on disk isn't in use, if it changed and didn't load.
    pub fn renewal_error(&self) -> Option<String> {
        self.failed.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|(_, why)| why.clone())
    }

    /// Read the file again; use it if it changed and loads. True if what the key shows changed:
    /// replaced, or its error appeared, changed or went.
    pub async fn reload(&self) -> bool {
        let before = self.renewal_error();
        let replaced = self.reload_file().await;
        replaced || self.renewal_error() != before
    }

    async fn reload_file(&self) -> bool {
        let Some(file) = &self.file else { return false };
        let secret = self.current().secret.is_some();
        let (outcome, seen) = match tokio::fs::read(file).await {
            Ok(bytes) => {
                let seen = hash(&[&bytes, self.passphrase.as_deref().unwrap_or("").as_bytes()]);
                if seen == self.current().hash {
                    *self.failed.lock().unwrap_or_else(|e| e.into_inner()) = None;
                    return false;
                }
                let passphrase = self.passphrase.clone();
                let loaded = tokio::task::spawn_blocking(move || load(&bytes, secret, passphrase.as_deref()))
                    .await
                    .unwrap_or_else(|e| Err(anyhow!("reading it stopped: {e}")));
                (loaded, seen)
            }
            Err(e) => (Err(anyhow!("can't read {}: {e}", file.display())), [0; 32]),
        };
        let outcome = outcome.and_then(|loaded| match loaded.about.not_after {
            Some(at) if at < Utc::now() => bail!("the new key expired on {}", at.format("%Y-%m-%d")),
            _ => Ok(loaded),
        });
        match outcome {
            Ok(loaded) => {
                tracing::info!(
                    key = %self.name,
                    fingerprint = %loaded.about.fingerprint,
                    user_id = %loaded.about.user_id,
                    expires = %loaded.about.not_after.map_or("never".into(), |at| at.format("%Y-%m-%d").to_string()),
                    "OpenPGP key replaced: the next file uses it"
                );
                *self.current.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(loaded);
                *self.failed.lock().unwrap_or_else(|e| e.into_inner()) = None;
                true
            }
            Err(e) => {
                let why = format!("{e:#}");
                let mut failed = self.failed.lock().unwrap_or_else(|e| e.into_inner());
                if failed.as_ref().is_none_or(|(h, w)| *h != seen || *w != why) {
                    tracing::error!(key = %self.name, error = %why, "the OpenPGP key file changed but can't be used: still using the one loaded before");
                }
                *failed = Some((seen, why));
                false
            }
        }
    }
}

/// Why a file a partner sent can't be taken: it isn't what `pgp:` asks for. Retrying won't
/// change it (the file, or our key, has to).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused(pub String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A writer that remembers whether writing failed, so `open` can tell its own disk's error (try
/// again) from the file's (refuse it).
struct Watched<W> {
    inner: W,
    failed: bool,
}

impl<W: Write> Write for Watched<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.write(buf).inspect_err(|_| self.failed = true)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush().inspect_err(|_| self.failed = true)
    }
}

/// Decrypt `input` with `decrypt` (ours) and check it's signed by `verify` (theirs), writing
/// the text to `out` as it goes: the number of bytes written, or `Refused` with why. Only an
/// error writing to `out` is an `Err`. Read `out` only once this returns `Ok(Ok(_))`: a refused
/// file may have written some of its text before its end showed it was wrong.
pub fn open(
    input: impl BufRead + std::fmt::Debug + Send,
    out: impl Write,
    decrypt: Option<&Key>,
    verify: Option<&Key>,
) -> Result<std::result::Result<u64, Refused>> {
    let mut out = Watched { inner: out, failed: false };
    let refused = |why: String| Ok(Err(Refused(why)));
    let (mut message, _) = match Message::from_reader(input) {
        Ok(m) => m,
        Err(e) => return refused(format!("it isn't an OpenPGP file (armored or binary): {e}")),
    };
    let ours = decrypt.map(Key::current);
    let theirs = verify.map(Key::current);
    match (&ours, message.is_encrypted()) {
        (Some(ours), true) => {
            let recipients = recipients(&message);
            let secret = ours.secret.as_ref().context("a decrypt key without its secret: checked at start")?;
            let empty = Password::empty();
            let ring = TheRing {
                secret_keys: vec![secret],
                key_passwords: vec![&empty],
                // The text goes to `out` as it's decrypted, and only counts once its end checks
                // out (MDC): no need to hold a whole file in memory first.
                decrypt_options: DecryptionOptions::new()
                    .enable_gnupg_aead()
                    .set_seipdv1_read_mode(Seipdv1ReadMode::Streaming),
                ..Default::default()
            };
            message = match message.decrypt_the_ring(ring, true) {
                Ok((m, _)) => m,
                Err(pgp::errors::Error::MissingKey) => {
                    return refused(format!(
                        "it isn't encrypted to our key `{}` ({}): it's for {recipients}. Did the partner use an old key of ours?",
                        decrypt.map_or("", |k| &k.name),
                        ours.about.named()
                    ));
                }
                Err(e) => return refused(format!("it couldn't be decrypted: {e}")),
            };
        }
        (Some(_), false) => {
            return refused(format!(
                "it isn't encrypted (sent in the clear), and `decrypt: {}` takes only encrypted files",
                decrypt.map_or("", |k| &k.name)
            ));
        }
        (None, true) => {
            return refused("it's encrypted: name our key in `pgp: { decrypt: ... }` to read it".into());
        }
        (None, false) => {}
    }
    while message.is_compressed() {
        message = match message.decompress() {
            Ok(m) => m,
            Err(e) => return refused(format!("its compressed contents can't be read: {e}")),
        };
    }
    if let (Some(key), false) = (verify, message.is_signed()) {
        return refused(format!("it isn't signed, and `verify: {}` takes only files signed by that key", key.name));
    }
    let written = match std::io::copy(&mut message, &mut out) {
        Ok(n) => n,
        Err(e) if out.failed => return Err(e).context("writing the decrypted text"),
        Err(e) => return refused(format!("it's damaged or was changed after it was made: {e}")),
    };
    if let (Some(key), Some(theirs)) = (verify, &theirs) {
        let mut keys: Vec<&dyn VerifyingKey> = vec![&theirs.public];
        keys.extend(theirs.public.public_subkeys.iter().map(|s| s as &dyn VerifyingKey));
        let results = message.verify_nested(&keys).map_err(|e| anyhow!("checking its signature: {e}"));
        match results {
            Ok(r) if r.iter().any(|r| matches!(r, VerificationResult::Valid(_))) => {}
            Ok(_) => {
                return refused(format!(
                    "its signature isn't by `{}` ({}), or doesn't match the file",
                    key.name,
                    theirs.about.named()
                ));
            }
            Err(e) => return refused(format!("{e:#}")),
        }
    }
    out.flush().context("writing the decrypted text")?;
    Ok(Ok(written))
}

/// Who an encrypted message is for: `key 0x…` for each recipient, as GnuPG names them.
fn recipients(message: &Message<'_>) -> String {
    let Message::Encrypted { esk, .. } = message else { return "nobody".into() };
    let ids: Vec<String> = esk
        .iter()
        .filter_map(|e| match e {
            Esk::PublicKeyEncryptedSessionKey(p) => {
                let hex: String = p.id().ok()?.as_ref().iter().map(|b| format!("{b:02X}")).collect();
                Some(format!("key 0x{hex}"))
            }
            Esk::SymKeyEncryptedSessionKey(_) => Some("a password".into()),
        })
        .collect();
    if ids.is_empty() { "an unknown key".into() } else { ids.join(", ") }
}

/// Sign `payload` with `sign` (ours) and encrypt it to `encrypt` (theirs): the file to write.
pub fn seal(payload: &[u8], encrypt: Option<&Key>, sign: Option<&Key>, armor: bool) -> Result<Vec<u8>> {
    let mut rng = rand::thread_rng();
    let ours = sign.map(Key::current);
    let theirs = encrypt.map(Key::current);
    let mut builder = MessageBuilder::from_bytes("", payload.to_vec());
    if let Some(ours) = &ours {
        let secret = ours.secret.as_ref().context("a sign key without its secret: checked at start")?;
        builder.sign(signing_key(secret)?, Password::empty(), HashAlgorithm::Sha256);
    }
    let out = match &theirs {
        Some(theirs) => {
            let mut builder = builder.seipd_v1(&mut rng, SymmetricKeyAlgorithm::AES256);
            match encryption_key(&theirs.public)? {
                Recipient::Primary(k) => builder.encrypt_to_key(&mut rng, k),
                Recipient::Subkey(k) => builder.encrypt_to_key(&mut rng, k),
            }
            .map_err(|e| anyhow!("encrypting to {}: {e}", theirs.about.named()))?;
            if armor {
                builder.to_armored_string(&mut rng, ArmorOptions::default()).map(String::into_bytes)
            } else {
                builder.to_vec(&mut rng)
            }
        }
        None if armor => builder.to_armored_string(&mut rng, ArmorOptions::default()).map(String::into_bytes),
        None => builder.to_vec(&mut rng),
    };
    out.map_err(|e| anyhow!("signing: {e}"))
}

/// A destination's `pgp:`, its keys loaded: what each file it writes goes through.
#[derive(Debug, Clone)]
pub struct Seal {
    encrypt: Option<Arc<Key>>,
    sign: Option<Arc<Key>>,
    armor: bool,
}

impl Seal {
    /// For `settings`, with the keys in `certificates` (the start checked they're there).
    pub fn new(settings: &ductus_core::pgp::Send, certificates: &crate::tls::Certificates) -> Result<Self> {
        let key = |name: &Option<String>| name.as_deref().map(|n| certificates.pgp_key(n).cloned()).transpose();
        Ok(Self { encrypt: key(&settings.encrypt)?, sign: key(&settings.sign)?, armor: settings.armor })
    }

    /// What a file's name gains: `pgp`, or `asc` when armored.
    pub fn extension(&self) -> &'static str {
        if self.armor { "asc" } else { "pgp" }
    }

    /// `payload` signed and encrypted, as the file to write.
    pub fn apply(&self, payload: &[u8]) -> Result<Vec<u8>> {
        seal(payload, self.encrypt.as_deref(), self.sign.as_deref(), self.armor)
    }
}

/// A source's `pgp:`, its keys loaded: what each file it picks up goes through.
#[derive(Debug, Clone)]
pub struct Opener {
    decrypt: Option<Arc<Key>>,
    verify: Option<Arc<Key>>,
}

impl Opener {
    /// For `settings`, with the keys in `certificates` (the start checked they're there).
    pub fn new(settings: &ductus_core::pgp::Receive, certificates: &crate::tls::Certificates) -> Result<Self> {
        let key = |name: &Option<String>| name.as_deref().map(|n| certificates.pgp_key(n).cloned()).transpose();
        Ok(Self { decrypt: key(&settings.decrypt)?, verify: key(&settings.verify)? })
    }

    /// `open` with these keys.
    pub fn open(
        &self,
        input: impl BufRead + std::fmt::Debug + Send,
        out: impl Write,
    ) -> Result<std::result::Result<u64, Refused>> {
        open(input, out, self.decrypt.as_deref(), self.verify.as_deref())
    }
}

enum Recipient<'a> {
    Primary(&'a SignedPublicKey),
    Subkey(&'a pgp::composed::SignedPublicSubKey),
}

/// The part of their key to encrypt to: the newest encryption subkey in force, else the
/// primary key if it may encrypt (as GnuPG picks).
fn encryption_key(key: &SignedPublicKey) -> Result<Recipient<'_>> {
    let now = Utc::now();
    let in_force =
        |created: DateTime<Utc>, binding: Option<&Signature>| expiry(created, binding).is_none_or(|at| at > now);
    let sub = key
        .public_subkeys
        .iter()
        .filter_map(|s| {
            let binding = newest(s.signatures.iter())?;
            let created = to_time(s.key.created_at());
            let (encrypt, _) = abilities(binding.key_flags(), s.key.algorithm());
            let usable =
                encrypt && binding.typ() != Some(SignatureType::SubkeyRevocation) && in_force(created, Some(binding));
            usable.then_some((created, s))
        })
        .max_by_key(|(created, _)| *created);
    if let Some((_, s)) = sub {
        return Ok(Recipient::Subkey(s));
    }
    let signature =
        newest(key.details.users.iter().flat_map(|u| u.signatures.iter()).chain(&key.details.direct_signatures));
    let (encrypt, _) = abilities(signature.map(Signature::key_flags).unwrap_or_default(), key.algorithm());
    if encrypt && in_force(to_time(key.primary_key.created_at()), signature) {
        return Ok(Recipient::Primary(key));
    }
    bail!("the key has no part that may encrypt, or all of them expired: ask the partner for a new key")
}

/// The part of our key to sign with: the newest signing subkey in force, else the primary key.
fn signing_key(key: &SignedSecretKey) -> Result<&dyn SigningKey> {
    let now = Utc::now();
    let sub = key
        .secret_subkeys
        .iter()
        .filter_map(|s| {
            let binding = newest(s.signatures.iter())?;
            let created = to_time(s.key.created_at());
            let (_, sign) = abilities(binding.key_flags(), s.key.algorithm());
            let usable = sign
                && binding.typ() != Some(SignatureType::SubkeyRevocation)
                && expiry(created, Some(binding)).is_none_or(|at| at > now);
            usable.then_some((created, s))
        })
        .max_by_key(|(created, _)| *created);
    if let Some((_, s)) = sub {
        return Ok(&s.key);
    }
    let public = key.to_public_key();
    let signature = newest(public.details.users.iter().flat_map(|u| u.signatures.iter()));
    let (_, sign) = abilities(signature.map(Signature::key_flags).unwrap_or_default(), public.algorithm());
    if sign {
        return Ok(&key.primary_key);
    }
    bail!("our key has no part that may sign, or all of them expired: make a signing subkey (`gpg --quick-add-key`)")
}

/// Keys made by tests, shaped as GnuPG makes them: a primary key that certifies and signs, and
/// an encryption subkey. (rPGP can't make a key that expires: tests of expiry use keys GnuPG
/// made, in `tests/fixtures/pgp`.)
#[cfg(test)]
pub mod testing {
    use pgp::composed::{EncryptionCaps, KeyType, SecretKeyParamsBuilder, SignedSecretKey, SubkeyParamsBuilder};
    use pgp::crypto::ecc_curve::ECCCurve;

    /// A secret key for `user_id`, locked with `passphrase` if given: armored.
    pub fn key(user_id: &str, passphrase: Option<&str>) -> String {
        let mut rng = rand::thread_rng();
        let mut sub = SubkeyParamsBuilder::default();
        sub.key_type(KeyType::ECDH(ECCCurve::Curve25519Legacy))
            .can_encrypt(EncryptionCaps::All)
            .passphrase(passphrase.map(str::to_string));
        let mut params = SecretKeyParamsBuilder::default();
        params
            .key_type(KeyType::Ed25519Legacy)
            .can_certify(true)
            .can_sign(true)
            .primary_user_id(user_id.into())
            .passphrase(passphrase.map(str::to_string))
            .subkeys(vec![sub.build().unwrap()]);
        let key: SignedSecretKey = params.build().unwrap().generate(&mut rng).unwrap();
        key.to_armored_string(Default::default()).unwrap()
    }

    /// The public key of a secret key `key` made: armored.
    pub fn public(secret: &str) -> String {
        use pgp::composed::Deserializable;
        let (key, _) = SignedSecretKey::from_armor_single(secret.as_bytes()).unwrap();
        key.to_public_key().to_armored_string(Default::default()).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ductus_core::pgp::PgpKey;

    /// A file in `tests/fixtures/pgp` (made by GnuPG 2.4: its `make.sh`).
    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pgp").join(name)
    }

    fn secret_file(name: &str, passphrase: Option<&str>) -> Result<Key> {
        let entry =
            PgpKey { key_file: Some(fixture(name)), passphrase: passphrase.map(Into::into), ..Default::default() };
        Key::load("ours", &entry)
    }

    fn public_file(name: &str) -> Result<Key> {
        Key::load("theirs", &PgpKey { public_key_file: Some(fixture(name)), ..Default::default() })
    }

    fn secret_text(text: &str, passphrase: Option<&str>) -> Key {
        let entry = PgpKey { key: Some(text.into()), passphrase: passphrase.map(Into::into), ..Default::default() };
        Key::load("ours", &entry).unwrap()
    }

    fn public_text(text: &str) -> Key {
        Key::load("theirs", &PgpKey { public_key: Some(text.into()), ..Default::default() }).unwrap()
    }

    fn opened(file: &str, decrypt: Option<&Key>, verify: Option<&Key>) -> std::result::Result<Vec<u8>, String> {
        let bytes = std::fs::read(fixture(file)).unwrap();
        open_bytes(&bytes, decrypt, verify)
    }

    fn open_bytes(bytes: &[u8], decrypt: Option<&Key>, verify: Option<&Key>) -> std::result::Result<Vec<u8>, String> {
        let mut out = Vec::new();
        let n = open(bytes, &mut out, decrypt, verify).unwrap().map_err(|r| r.0)?;
        assert_eq!(n as usize, out.len());
        Ok(out)
    }

    /// GnuPG's keys load: ours unlocked with its passphrase (only that one), a partner's armored
    /// or binary; each in the wrong slot, or without its passphrase, refused saying why.
    #[test]
    fn gnupg_keys_load_and_wrong_ones_say_why() {
        let ours = secret_file("ours.sec.asc", Some("ductus-test")).unwrap().about();
        assert_eq!(
            (ours.user_id.as_str(), ours.secret, ours.not_after),
            ("Ductus Test <ductus@example.org>", true, None)
        );
        assert_eq!(ours.fingerprint.len(), 40, "v4: SHA-1, 40 hex digits");
        let payer = public_file("payer.pub.asc").unwrap().about();
        assert_eq!((payer.user_id.as_str(), payer.secret), ("Payer EDI <edi@payer.example>", false));
        assert_eq!(public_file("payer.pub.gpg").unwrap().about(), payer, "binary reads the same as armored");

        let err = |r: Result<Key>| format!("{:#}", r.unwrap_err());
        assert!(err(secret_file("ours.sec.asc", None)).contains("has a passphrase: give it in `passphrase:`"));
        assert!(err(secret_file("ours.sec.asc", Some("wrong"))).contains("its passphrase didn't unlock it"));
        assert!(err(secret_file("ours.pub.asc", None)).contains("this is a public key; ours must be a secret key"));
        assert!(err(public_file("payer.sec.asc")).contains("this is a secret key: a partner's public key goes here"));
        assert!(err(public_file("batch.hl7")).contains("not an OpenPGP public key"));
    }

    /// A key's expiry is when it can no longer do what it does: here its encryption subkey's
    /// (the primary key never expires). One already expired doesn't load.
    #[test]
    fn a_key_expires_when_its_last_usable_part_does() {
        let dated = public_file("dated.pub.asc").unwrap().about();
        assert_eq!(dated.not_after.map(|t| t.format("%Y-%m-%d").to_string()).as_deref(), Some("2099-01-01"));
        assert_eq!(dated.earliest.as_deref(), Some("its encryption subkey"));
        let err = format!("{:#}", public_file("expired.pub.asc").unwrap_err());
        assert!(err.starts_with("it expired on 2021-01-01 (0x"), "{err}");
        assert!(err.contains("Expired <expired@example.org>"), "{err}");
    }

    /// An older encryption subkey ending while a newer one goes on ends nothing; the primary key
    /// ending ends everything.
    #[test]
    fn an_old_subkey_ending_beside_a_new_one_ends_nothing() {
        let at =
            |y: i32| Some(chrono::NaiveDate::from_ymd_opt(y, 1, 1).unwrap().and_hms_opt(0, 0, 0).unwrap().and_utc());
        let part =
            |what, expires, encrypt, sign| Component { what, created: at(2020).unwrap(), expires, encrypt, sign };
        let primary = |expires| part("its primary key", expires, false, true);
        let old = part("its encryption subkey", at(2027), true, false);
        let new = part("its encryption subkey", at(2031), true, false);
        assert_eq!(until(&[primary(None), old, new]), (at(2031), Some("its encryption subkey".into())));
        let never = part("its encryption subkey", None, true, false);
        let old = part("its encryption subkey", at(2027), true, false);
        assert_eq!(until(&[primary(None), old, never]), (None, None), "one that never expires");
        assert_eq!(until(&[primary(at(2026)), part("its encryption subkey", at(2031), true, false)]), (at(2026), None));
    }

    /// What GnuPG sends (`gpg --sign --encrypt`, compressed inside, binary or armored) opens
    /// to the text it was made from.
    #[test]
    fn what_gnupg_encrypts_and_signs_opens() {
        let ours = secret_file("ours.sec.asc", Some("ductus-test")).unwrap();
        let payer = public_file("payer.pub.asc").unwrap();
        let text = std::fs::read(fixture("batch.hl7")).unwrap();
        for file in ["from-payer.hl7.pgp", "from-payer.hl7.asc"] {
            assert_eq!(opened(file, Some(&ours), Some(&payer)).as_deref(), Ok(&text[..]), "{file}");
            assert_eq!(opened(file, Some(&ours), None).as_deref(), Ok(&text[..]), "{file}: without verify:");
        }
    }

    /// Each way a file isn't what `pgp:` asks for is refused with words that say which.
    #[test]
    fn files_that_aren_t_what_pgp_asks_for_are_refused_saying_why() {
        let ours = secret_file("ours.sec.asc", Some("ductus-test")).unwrap();
        let payer = public_file("payer.pub.asc").unwrap();
        let dated = public_file("dated.pub.asc").unwrap();
        let why = |r: std::result::Result<Vec<u8>, String>| r.unwrap_err();

        let unsigned = why(opened("unsigned.hl7.pgp", Some(&ours), Some(&payer)));
        assert_eq!(unsigned, "it isn't signed, and `verify: theirs` takes only files signed by that key");
        assert!(opened("unsigned.hl7.pgp", Some(&ours), None).is_ok(), "no verify: unsigned is fine");
        let other = why(opened("from-payer.hl7.pgp", Some(&ours), Some(&dated)));
        assert!(other.starts_with("its signature isn't by `theirs` (0x"), "{other}");
        let not_ours = why(opened("not-for-us.hl7.pgp", Some(&ours), None));
        assert!(not_ours.starts_with("it isn't encrypted to our key `ours` (0x"), "{not_ours}");
        assert!(not_ours.contains("it's for key 0x"), "{not_ours}");
        let clear = why(opened("batch.hl7", Some(&ours), None));
        assert!(clear.starts_with("it isn't an OpenPGP file"), "{clear}");
        let encrypted = why(opened("from-payer.hl7.pgp", None, Some(&payer)));
        assert_eq!(encrypted, "it's encrypted: name our key in `pgp: { decrypt: ... }` to read it");

        // Signed but not encrypted, with `decrypt:`: sent in the clear.
        let signer = secret_text(&testing::key("Signer <s@example.org>", None), None);
        let signed_only = seal(b"MSH|", None, Some(&signer), false).unwrap();
        assert!(why(open_bytes(&signed_only, Some(&ours), None)).starts_with("it isn't encrypted (sent in the clear)"));

        // A byte changed in the middle: the integrity check at its end catches it.
        let mut damaged = std::fs::read(fixture("from-payer.hl7.pgp")).unwrap();
        let middle = damaged.len() / 2;
        damaged[middle] ^= 0x40;
        assert!(open_bytes(&damaged, Some(&ours), Some(&payer)).is_err(), "damaged");
    }

    /// What we seal opens again, signed and encrypted, binary and armored, with a key that has
    /// a passphrase; only the recipient's key opens it.
    #[test]
    fn what_we_seal_opens_for_the_recipient_only() {
        let ours = secret_text(&testing::key("Hospital <edi@hospital.example>", Some("pw")), Some("pw"));
        let theirs_secret = testing::key("Payer <edi@payer.example>", None);
        let theirs = public_text(&testing::public(&theirs_secret));
        let at_payer = secret_text(&theirs_secret, None);
        let our_public = public_text(&testing::public(&testing::key("x <x@example.org>", None)));
        let hospital_public = {
            let text = testing::public(
                &ours.current().secret.as_ref().unwrap().to_armored_string(Default::default()).unwrap(),
            );
            public_text(&text)
        };
        for armor in [false, true] {
            let file = seal(b"MSH|^~\\&|A\rPID|1||9\r", Some(&theirs), Some(&ours), armor).unwrap();
            assert_eq!(file.starts_with(b"-----BEGIN PGP MESSAGE-----"), armor);
            let text = open_bytes(&file, Some(&at_payer), Some(&hospital_public)).unwrap();
            assert_eq!(text, b"MSH|^~\\&|A\rPID|1||9\r");
            assert!(open_bytes(&file, Some(&ours), None).is_err(), "not encrypted to us");
            assert!(open_bytes(&file, Some(&at_payer), Some(&our_public)).is_err(), "signed by someone else");
        }
        let plain_signed = seal(b"x", None, Some(&ours), false).unwrap();
        assert_eq!(open_bytes(&plain_signed, None, Some(&hospital_public)).unwrap(), b"x");
    }

    /// Our own disk failing while the text is written is an error (tried again), not the file's
    /// fault.
    #[test]
    fn a_failing_disk_is_an_error_not_a_refusal() {
        struct Full;
        impl Write for Full {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("No space left on device"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let ours = secret_file("ours.sec.asc", Some("ductus-test")).unwrap();
        let bytes = std::fs::read(fixture("from-payer.hl7.pgp")).unwrap();
        let err = open(&bytes[..], Full, Some(&ours), None).unwrap_err();
        assert!(format!("{err:#}").contains("No space left on device"), "{err:#}");
    }

    /// What OpenPGP costs (ignored; `cargo test --release -p ductus-runtime --lib measure_pgp --
    /// --ignored --nocapture`): opening a 100 MB signed and encrypted file (to a file, as pickup
    /// does), and sealing one HL7 message (sign + encrypt), with GnuPG's kind of keys (Ed25519,
    /// Curve25519) and with RSA 3072 (the payer's fixture).
    #[test]
    #[ignore]
    fn measure_pgp() {
        let ours = secret_text(&testing::key("Hospital <h@example.org>", None), None);
        let ours_public = public_text(&testing::public(
            &ours.current().secret.as_ref().unwrap().to_armored_string(Default::default()).unwrap(),
        ));
        let rsa = public_file("payer.pub.asc").unwrap();
        let line = b"MSH|^~\\&|LAB|H|EMR|H|20261121||ORU^R01|C1|P|2.5\rPID|1||100000\rOBX|1|NM|GLU||93\r";
        let big: Vec<u8> = line.iter().copied().cycle().take(100 << 20).collect();
        let sealed = seal(&big, Some(&ours_public), Some(&ours), false).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let started = std::time::Instant::now();
        let out = std::io::BufWriter::new(std::fs::File::create(dir.path().join("text")).unwrap());
        let n = open(std::io::BufReader::new(&sealed[..]), out, Some(&ours), Some(&ours_public)).unwrap().unwrap();
        let seconds = started.elapsed().as_secs_f64();
        println!("open 100 MB (decrypt + verify, to a file): {:.2}s = {:.0} MB/s", seconds, n as f64 / seconds / 1e6);
        for (what, to) in [("Curve25519", &ours_public), ("RSA 3072", &rsa)] {
            let started = std::time::Instant::now();
            for _ in 0..1000 {
                seal(line, Some(to), Some(&ours), false).unwrap();
            }
            println!(
                "seal one message (Ed25519 sign + {what} encrypt): {:.0} µs",
                started.elapsed().as_secs_f64() * 1e3
            );
        }
    }

    /// A replaced key file is used from the next file on; one that can't be used keeps the old
    /// key, with why.
    #[tokio::test]
    async fn a_replaced_key_file_is_used_without_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("payer.asc");
        std::fs::copy(fixture("payer.pub.asc"), &path).unwrap();
        let key = Key::load("payer", &PgpKey { public_key_file: Some(path.clone()), ..Default::default() }).unwrap();
        let before = key.about();
        assert!(!key.reload().await, "unchanged");
        std::fs::copy(fixture("dated.pub.asc"), &path).unwrap();
        assert!(key.reload().await);
        assert_eq!(key.about().user_id, "Dated <dated@example.org>");
        std::fs::copy(fixture("expired.pub.asc"), &path).unwrap();
        assert!(key.reload().await, "its error appeared");
        assert_eq!(key.renewal_error().as_deref(), Some("the new key expired on 2021-01-01"));
        assert_eq!(key.about().user_id, "Dated <dated@example.org>", "still the one before");
        assert!(!key.reload().await, "the same error again changes nothing");
        assert_ne!(before.fingerprint, key.about().fingerprint);
    }
}
