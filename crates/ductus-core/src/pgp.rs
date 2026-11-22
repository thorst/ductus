//! OpenPGP on files (0.7.8.1): the keys resources.yaml holds (`pgp:`), and what file and SFTP
//! sources and destinations do with them.
//!
//! ```yaml
//! # resources.yaml
//! pgp:
//!   ours:                                   # our key: decrypts what partners send, signs what we send
//!     key_file: /etc/ductus/pgp/ours.asc    #   a secret key, armored or binary (or `key:` from a secret)
//!     passphrase: ${secret:PGP_PASSPHRASE}  #   if it has one
//!   payer:                                  # a partner's public key: encrypts to them, checks their signature
//!     public_key_file: pgp/payer.asc
//!
//! # interfaces/<name>/interface.yaml
//! source:
//!   sftp: { server: payer_sftp, dir: outbound, pgp: { decrypt: ours, verify: payer } }
//! destinations:
//!   payer: { sftp: { server: payer_sftp, dir: inbound, pgp: { encrypt: payer, sign: ours } } }
//! ```
//!
//! A file that doesn't decrypt or verify is stored quarantined, as received, with why, and the
//! folder goes on to the next file. The keys' expiry is watched like the certificates' (the
//! Doctor page's "Ours", and `certificate` alerts).

use crate::InterfaceConfig;
use crate::config::{Destination, Source};
use crate::resources::Resources;
use crate::yaml::Problem;
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// A `pgp:` entry in resources.yaml: our secret key (`key_file` or `key`), or a partner's public
/// key (`public_key_file` or `public_key`).
#[derive(Clone, Default, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct PgpKey {
    /// A file with our secret key, armored (`-----BEGIN PGP PRIVATE KEY BLOCK-----`, what `gpg
    /// --export-secret-keys --armor` writes) or binary. It decrypts files partners encrypted to
    /// us (`decrypt:`) and signs the files we send (`sign:`). Re-read when it changes, checked
    /// every minute: replace the file to change keys, with no restart. Readable by the account
    /// the engine runs as and nobody else. A relative path is read from the project folder.
    /// Example: `key_file: /etc/ductus/pgp/ours.asc`.
    #[serde(default)]
    pub key_file: Option<PathBuf>,
    /// Our secret key as text (armored), usually from a secret (`${secret:PGP_KEY}`), for
    /// servers where keys live in a vault rather than in files. Read at start only. Example:
    /// `key: "${secret:PGP_KEY}"`.
    #[serde(default)]
    pub key: Option<String>,
    /// The passphrase that unlocks the secret key, from a secret, when it has one (GnuPG asks
    /// for one by default). It's used once, as the key is read: a wrong one stops the start
    /// saying so. Default: none. Example: `passphrase: "${secret:PGP_PASSPHRASE}"`.
    #[serde(default)]
    pub passphrase: Option<String>,
    /// A file with a partner's public key (armored or binary, what `gpg --export --armor`
    /// writes): files we send them are encrypted to it (`encrypt:`), and files they send us
    /// must be signed by it (`verify:`). Re-read when it changes, every minute. Example:
    /// `public_key_file: pgp/payer.asc`.
    #[serde(default)]
    pub public_key_file: Option<PathBuf>,
    /// A partner's public key as text (armored). Read at start only. Example: `public_key: |
    /// -----BEGIN PGP PUBLIC KEY BLOCK----- ...`.
    #[serde(default)]
    pub public_key: Option<String>,
    /// The application the key belongs to, for `ductus impact`. Default: none. Example:
    /// `application: Claims clearinghouse`.
    #[serde(default)]
    pub application: Option<String>,
    /// The vendor the key belongs to (the partner, for theirs), for `ductus impact`. Default:
    /// none. Example: `vendor: Availity`.
    #[serde(default)]
    pub vendor: Option<String>,
    /// Who renews this key, best as an email address: alert rules with `to: owner` mail them as
    /// it nears expiry. Default: the owner of the one interface that uses it, if just one does.
    /// Example: `owner: edi-team@hospital.org`.
    #[serde(default)]
    pub owner: Option<String>,
    /// Your company's documentation fields for this entry (those whose `applies_to` in
    /// `docs-schema.yaml` includes `resource`). Default: none. Example: `custom: { ticket:
    /// CHG-1234 }`.
    #[serde(default)]
    pub custom: crate::docs::Custom,
}

/// Where a `pgp:` entry's key comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyText<'a> {
    SecretFile(&'a Path),
    Secret(&'a str),
    PublicFile(&'a Path),
    Public(&'a str),
}

impl KeyText<'_> {
    /// A secret key (ours), not a public one.
    pub fn is_secret(self) -> bool {
        matches!(self, KeyText::SecretFile(_) | KeyText::Secret(_))
    }
}

impl PgpKey {
    /// Where its key is; None if the entry gives none, or more than one (`problem` says so).
    pub fn text(&self) -> Option<KeyText<'_>> {
        match (&self.key_file, &self.key, &self.public_key_file, &self.public_key) {
            (Some(f), None, None, None) => Some(KeyText::SecretFile(f)),
            (None, Some(k), None, None) => Some(KeyText::Secret(k)),
            (None, None, Some(f), None) => Some(KeyText::PublicFile(f)),
            (None, None, None, Some(k)) => Some(KeyText::Public(k)),
            _ => None,
        }
    }

    /// Our secret key (`key_file` or `key`).
    pub fn is_secret(&self) -> bool {
        self.text().is_some_and(KeyText::is_secret)
    }

    /// The file its key is read from (re-read every minute), if any.
    pub fn file(&self) -> Option<&Path> {
        match self.text()? {
            KeyText::SecretFile(f) | KeyText::PublicFile(f) => Some(f),
            KeyText::Secret(_) | KeyText::Public(_) => None,
        }
    }

    pub(crate) fn problem(&self) -> Option<&'static str> {
        if self.text().is_none() {
            return Some(
                "give one of `key_file` or `key` (our secret key), or `public_key_file` or `public_key` (a partner's)",
            );
        }
        if self.passphrase.is_some() && !self.is_secret() {
            return Some("`passphrase` unlocks a secret key (`key_file` or `key`); a public key has none");
        }
        None
    }

    /// Relative file names are read from the project folder.
    pub(crate) fn resolve(&mut self, root: &Path) {
        for file in [&mut self.key_file, &mut self.public_key_file].into_iter().flatten() {
            if file.is_relative() {
                *file = root.join(&*file);
            }
        }
    }
}

// Hand-written so secret keys and passphrases never end up in logs.
impl std::fmt::Debug for PgpKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PgpKey")
            .field("key_file", &self.key_file)
            .field("public_key_file", &self.public_key_file)
            .field("secret", &self.is_secret())
            .finish_non_exhaustive()
    }
}

/// `pgp:` on a file or SFTP source: what is done with each file before it's read.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Receive {
    /// Decrypt each file with this key of ours (a `pgp:` entry with `key_file` or `key`). A file
    /// that isn't encrypted, or isn't encrypted to this key, is stored quarantined with why, and
    /// the next file is read. Armored and binary files both work. Default: files aren't
    /// decrypted. Example: `decrypt: ours`.
    #[serde(default)]
    pub decrypt: Option<String>,
    /// Accept only files signed by this partner's key (a `pgp:` entry): a file with no
    /// signature, or a signature by any other key, or one that doesn't match the file, is
    /// stored quarantined with why. Default: signatures aren't checked. Example: `verify:
    /// payer`.
    #[serde(default)]
    pub verify: Option<String>,
}

/// `pgp:` on a file or SFTP destination: what is done with each file before it's written.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Send {
    /// Encrypt each file to this partner's key (a `pgp:` entry): only they can read it. The
    /// file's name gains `.pgp` (`.asc` with `armor: true`). Default: not encrypted. Example:
    /// `encrypt: payer`.
    #[serde(default)]
    pub encrypt: Option<String>,
    /// Sign each file with this key of ours (a `pgp:` entry with `key_file` or `key`), so the
    /// partner can tell it came from us. Default: not signed. Example: `sign: ours`.
    #[serde(default)]
    pub sign: Option<String>,
    /// Write the file as ASCII armor (`-----BEGIN PGP MESSAGE-----`, named `.asc`) rather than
    /// binary (`.pgp`), for partners whose tools ask for it. Default: `false`. Example: `armor:
    /// true`.
    #[serde(default)]
    pub armor: bool,
}

impl Send {
    /// What the file's name gains: `pgp` or `asc`.
    pub fn extension(&self) -> &'static str {
        if self.armor { "asc" } else { "pgp" }
    }
}

/// What a key is used for, at one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Use {
    Decrypt,
    Verify,
    Encrypt,
    Sign,
}

impl Use {
    /// `decrypt`, `verify`, `encrypt`, `sign`: the setting's name.
    pub fn setting(self) -> &'static str {
        match self {
            Use::Decrypt => "decrypt",
            Use::Verify => "verify",
            Use::Encrypt => "encrypt",
            Use::Sign => "sign",
        }
    }

    /// Needs our secret key, not a public one.
    pub fn needs_secret(self) -> bool {
        matches!(self, Use::Decrypt | Use::Sign)
    }
}

/// One place an interface uses a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyUse<'a> {
    /// The `pgp:` entry.
    pub key: &'a str,
    pub interface: &'a str,
    pub what: Use,
    /// `source`, or `destination `payer``.
    pub place: String,
    /// The setting's key in interface.yaml.
    pub at: Vec<&'a str>,
}

impl KeyUse<'_> {
    /// How the Doctor page lists it: `decrypts files `claims-in` picks up`.
    pub fn described(&self) -> String {
        let i = self.interface;
        match self.what {
            Use::Decrypt => format!("decrypts files `{i}` picks up"),
            Use::Verify => format!("checks files `{i}` picks up are signed by it"),
            Use::Encrypt => format!("encrypts files `{i}` sends ({})", self.place),
            Use::Sign => format!("signs files `{i}` sends ({})", self.place),
        }
    }
}

/// The `pgp:` settings of a source or destination.
pub fn on_source(source: &Source) -> Option<&Receive> {
    match source {
        Source::File(f) | Source::Sftp(f) => f.pgp.as_ref(),
        _ => None,
    }
}

/// The `pgp:` settings of a destination.
pub fn on_destination(destination: &Destination) -> Option<&Send> {
    match destination {
        Destination::File { pgp, .. } | Destination::Sftp { pgp, .. } => pgp.as_ref(),
        _ => None,
    }
}

/// Every key the interfaces use, where and for what, in the order written.
pub fn uses<'a>(configs: impl IntoIterator<Item = &'a InterfaceConfig>) -> Vec<KeyUse<'a>> {
    let mut out = Vec::new();
    for c in configs {
        let interface = c.name.as_str();
        if let Some(r) = on_source(&c.source) {
            let kind = c.source.kind();
            for (key, what) in [(&r.decrypt, Use::Decrypt), (&r.verify, Use::Verify)] {
                if let Some(key) = key {
                    let at = vec!["source", kind, "pgp", what.setting()];
                    out.push(KeyUse { key, interface, what, place: "source".into(), at });
                }
            }
        }
        for (name, d) in c.destinations() {
            let Some(s) = on_destination(&d.connector) else { continue };
            let kind = if matches!(d.connector, Destination::File { .. }) { "file" } else { "sftp" };
            for (key, what) in [(&s.encrypt, Use::Encrypt), (&s.sign, Use::Sign)] {
                if let Some(key) = key {
                    let at = vec!["destinations", name, kind, "pgp", what.setting()];
                    out.push(KeyUse { key, interface, what, place: format!("destination `{name}`"), at });
                }
            }
        }
    }
    out
}

/// Problems with the interfaces' `pgp:` settings: a partner's public key where our secret key
/// is needed (`decrypt`, `sign`), and a `pgp:` that names no key. (A key resources.yaml doesn't
/// define is a missing reference, like any resource: `Resources::missing_references`.)
pub fn check<'a>(configs: impl IntoIterator<Item = &'a InterfaceConfig>, resources: &Resources) -> Vec<Problem> {
    let configs: Vec<&InterfaceConfig> = configs.into_iter().collect();
    let mut problems = Vec::new();
    for c in &configs {
        if let Some(r) = on_source(&c.source)
            && r.decrypt.is_none()
            && r.verify.is_none()
        {
            let text = format!("interface `{}`: source: `pgp:` names no key: give `decrypt`, `verify` or both", c.name);
            problems.push(Problem::new(text, &c.name, &["source", c.source.kind(), "pgp"]));
        }
        for (name, d) in c.destinations() {
            if let Some(s) = on_destination(&d.connector)
                && s.encrypt.is_none()
                && s.sign.is_none()
            {
                let text = format!(
                    "interface `{}`: destination `{name}`: `pgp:` names no key: give `encrypt`, `sign` or both",
                    c.name
                );
                let kind = if matches!(d.connector, Destination::File { .. }) { "file" } else { "sftp" };
                problems.push(Problem::new(text, &c.name, &["destinations", name, kind, "pgp"]));
            }
        }
    }
    for u in uses(configs.iter().copied()) {
        if let Some(k) = resources.pgp.get(u.key)
            && u.what.needs_secret()
            && !k.is_secret()
        {
            let text = format!(
                "interface `{}`: {} pgp.{}: `{}` is a partner's public key; to {} takes our secret key (a `pgp:` entry with `key_file` or `key`)",
                u.interface,
                u.place,
                u.what.setting(),
                u.key,
                u.what.setting()
            );
            problems.push(Problem::new(text, u.interface, &u.at));
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::Environment;

    fn resources(text: &str) -> Result<Resources, String> {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("resources.yaml"), text).unwrap();
        let env = Environment::with_variables("dev", &[("PW", "secret-passphrase")]);
        let r = Resources::load(dir.path(), &env).map_err(|e| format!("{e:#}"));
        // Relative key files are the project folder's.
        if let Ok(r) = &r
            && let Some(f) = r.pgp.get("payer").and_then(PgpKey::file)
        {
            assert_eq!(f, dir.path().join("pgp/payer.asc"));
        }
        r
    }

    fn config(yaml: &str) -> InterfaceConfig {
        crate::yaml::from_value(crate::yaml::parse(yaml).unwrap()).unwrap()
    }

    /// Our secret key or a partner's public key, one way each; a passphrase only on ours; never
    /// a certificate's name; the passphrase stays out of logs.
    #[test]
    fn a_pgp_entry_is_one_key_by_a_name_of_its_own() {
        let r = resources(
            "pgp:\n  ours: { key_file: /k/ours.asc, passphrase: '${secret:PW}', owner: EDI }\n  payer: { public_key_file: pgp/payer.asc }\n",
        );
        let Err(e) = &r else { unreachable!("no secrets set up: refused") };
        assert!(e.contains("PW"), "{e}");
        let r = resources(
            "pgp:\n  ours: { key_file: /k/ours.asc, passphrase: '${PW}', owner: EDI }\n  payer: { public_key_file: pgp/payer.asc }\n",
        )
        .unwrap();
        assert!(r.pgp["ours"].is_secret() && !r.pgp["payer"].is_secret());
        assert_eq!(r.owner("ours"), Some("EDI"));
        assert!(r.contains(crate::resources::Kind::Pgp, "payer"));
        assert!(!format!("{:?}", r.pgp).contains("secret-passphrase"), "passphrases stay out of logs");

        let refused = |text: &str| resources(text).unwrap_err();
        assert!(refused("pgp:\n  x: {}\n").contains("pgp key `x`: give one of `key_file` or `key`"));
        assert!(refused("pgp:\n  x: { key_file: a, public_key_file: b }\n").contains("give one of"));
        assert!(refused("pgp:\n  x: { public_key: K, passphrase: p }\n").contains("a public key has none"));
        let clash = refused("certificates:\n  x: { ca: CA }\npgp:\n  x: { public_key: K }\n");
        assert!(clash.contains("`x` is both a `certificates:` and a `pgp:` entry"), "{clash}");
    }

    /// Where each key is used, and the settings that can't work: a public key to decrypt or sign
    /// with, a `pgp:` naming no key. A missing key is a missing reference, like any resource.
    #[test]
    fn decrypt_and_sign_need_our_secret_key() {
        let r = resources("pgp:\n  ours: { key: K }\n  payer: { public_key: K }\n").unwrap();
        let good = config(
            "name: claims\nsource: { file: { dir: in, pgp: { decrypt: ours, verify: payer } } }\ndestinations:\n  out: { sftp: { server: s, dir: up, pgp: { encrypt: payer, sign: ours } } }\n",
        );
        let used: Vec<String> = uses([&good]).iter().map(|u| format!("{} {}", u.key, u.described())).collect();
        assert_eq!(
            used,
            [
                "ours decrypts files `claims` picks up",
                "payer checks files `claims` picks up are signed by it",
                "payer encrypts files `claims` sends (destination `out`)",
                "ours signs files `claims` sends (destination `out`)"
            ]
        );
        assert!(check([&good], &r).is_empty());

        let wrong = config(
            "name: claims\nsource: { file: { dir: in, pgp: { decrypt: payer } } }\ndestinations:\n  out: { file: { dir: o, pgp: { sign: payer } } }\n  none: { file: { dir: p, pgp: {} } }\n  lost: { file: { dir: q, pgp: { encrypt: nobody } } }\n",
        );
        let problems: Vec<(String, Vec<String>)> = check([&wrong], &r).into_iter().map(|p| (p.text, p.key)).collect();
        assert_eq!(problems.len(), 3, "{problems:#?}");
        assert_eq!(
            problems[0].0,
            "interface `claims`: destination `none`: `pgp:` names no key: give `encrypt`, `sign` or both"
        );
        assert_eq!(problems[0].1, ["destinations", "none", "file", "pgp"]);
        assert!(problems[1].0.starts_with("interface `claims`: source pgp.decrypt: `payer` is a partner's public key"));
        assert_eq!(problems[1].1, ["source", "file", "pgp", "decrypt"]);
        assert!(problems[2].0.contains("destination `out` pgp.sign: `payer` is a partner's public key; to sign takes"));
        let missing = r.missing_references([&wrong]);
        assert_eq!(missing.len(), 1);
        assert!(
            missing[0].text.contains("destination `lost` pgp.encrypt: no pgp key named `nobody`"),
            "{}",
            missing[0]
        );
    }
}
