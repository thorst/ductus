//! Built-in secret store: `secrets/<env>.json`, values encrypted with ChaCha20-Poly1305.
//!
//! The file is safe to commit: without the key it reveals only secret *names*. The key comes
//! from the `DUCTUS_SECRET_KEY` environment variable (base64, 32 bytes; create one with
//! `ductus secret init`). Each value is bound to its environment and name, so ciphertext
//! can't be copied to another name or environment. External vaults come later (roadmap v1.4).

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const KEY_VAR: &str = "DUCTUS_SECRET_KEY";

pub struct SecretStore {
    env: String,
    path: PathBuf,
    values: BTreeMap<String, String>,
}

fn file(root: &Path, env: &str) -> PathBuf {
    root.join("secrets").join(format!("{env}.json"))
}

fn cipher(key_b64: &str) -> Result<ChaCha20Poly1305> {
    let key = B64.decode(key_b64.trim()).context("secret key is not valid base64")?;
    if key.len() != 32 {
        bail!("secret key must be 32 bytes (got {})", key.len());
    }
    let key: [u8; 32] = key.try_into().expect("length checked");
    Ok(ChaCha20Poly1305::new(&Key::from(key)))
}

fn random<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).expect("OS random number generator");
    bytes
}

fn key_from_env() -> Result<String> {
    std::env::var(KEY_VAR).map_err(|_| anyhow!("{KEY_VAR} is not set; it's needed to read encrypted secrets"))
}

/// A new random key, base64-encoded.
pub fn generate_key() -> String {
    B64.encode(random::<32>())
}

impl SecretStore {
    /// Open the store for `env`, if its file exists.
    pub fn open(root: &Path, env: &str) -> Result<Option<Self>> {
        let path = file(root, env);
        if !path.is_file() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let values = serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        Ok(Some(Self { env: env.to_string(), path, values }))
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.values.keys().map(String::as_str)
    }

    /// Decrypt a secret. `Ok(None)` if there's no secret with that name.
    pub fn get(&self, name: &str) -> Result<Option<String>> {
        let Some(sealed) = self.values.get(name) else { return Ok(None) };
        self.decrypt(name, sealed, &key_from_env()?).map(Some)
    }

    fn aad(&self, name: &str) -> String {
        format!("ductus/{}/{name}", self.env)
    }

    fn decrypt(&self, name: &str, sealed: &str, key: &str) -> Result<String> {
        let bytes = B64.decode(sealed).with_context(|| format!("secret `{name}` is corrupt"))?;
        if bytes.len() < 12 {
            bail!("secret `{name}` is corrupt");
        }
        let (nonce, ciphertext) = bytes.split_at(12);
        let nonce: [u8; 12] = nonce.try_into().expect("length checked");
        let aad = self.aad(name);
        let plain = cipher(key)?
            .decrypt(&Nonce::from(nonce), Payload { msg: ciphertext, aad: aad.as_bytes() })
            .map_err(|_| anyhow!("can't decrypt secret `{name}`: wrong {KEY_VAR}, or the value was altered"))?;
        String::from_utf8(plain).context("secret is not UTF-8")
    }

    /// Create or update a secret and save the file.
    pub fn set(root: &Path, env: &str, name: &str, value: &str, key: &str) -> Result<()> {
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            bail!("secret names use letters, digits and _ only");
        }
        let mut store = Self::open(root, env)?.unwrap_or_else(|| Self {
            env: env.to_string(),
            path: file(root, env),
            values: BTreeMap::new(),
        });
        // Refuse to mix keys in one file: an existing secret must decrypt with this key.
        if let Some((existing, sealed)) = store.values.iter().next() {
            store.decrypt(existing, sealed, key).context("this key doesn't match the existing secrets file")?;
        }
        let nonce = random::<12>();
        let aad = store.aad(name);
        let ciphertext = cipher(key)?
            .encrypt(&Nonce::from(nonce), Payload { msg: value.as_bytes(), aad: aad.as_bytes() })
            .map_err(|_| anyhow!("encryption failed"))?;
        let mut sealed = nonce.to_vec();
        sealed.extend_from_slice(&ciphertext);
        store.values.insert(name.to_string(), B64.encode(sealed));

        std::fs::create_dir_all(store.path.parent().unwrap())?;
        std::fs::write(&store.path, serde_json::to_string_pretty(&store.values)? + "\n")
            .with_context(|| format!("writing {}", store.path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_binds_values_to_their_name_and_environment() {
        let dir = tempfile::tempdir().unwrap();
        let key = generate_key();
        SecretStore::set(dir.path(), "prod", "DB_PASSWORD", "p@ss: #1", &key).unwrap();
        SecretStore::set(dir.path(), "prod", "API_KEY", "abc", &key).unwrap();

        let store = SecretStore::open(dir.path(), "prod").unwrap().unwrap();
        assert_eq!(store.names().collect::<Vec<_>>(), ["API_KEY", "DB_PASSWORD"]);
        assert_eq!(store.decrypt("DB_PASSWORD", &store.values["DB_PASSWORD"], &key).unwrap(), "p@ss: #1");
        // The file never contains the plaintext.
        let raw = std::fs::read_to_string(dir.path().join("secrets/prod.json")).unwrap();
        assert!(!raw.contains("p@ss"));

        // Ciphertext moved to another name doesn't decrypt.
        assert!(store.decrypt("API_KEY", &store.values["DB_PASSWORD"], &key).is_err());
        // Wrong key doesn't decrypt, and can't be used to add to the file.
        let other = generate_key();
        assert!(store.decrypt("DB_PASSWORD", &store.values["DB_PASSWORD"], &other).is_err());
        assert!(SecretStore::set(dir.path(), "prod", "X", "y", &other).is_err());
        // Missing file: no store.
        assert!(SecretStore::open(dir.path(), "dev").unwrap().is_none());
    }
}
