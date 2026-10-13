//! Passwords for local user accounts: hashed with argon2id (the library's defaults are OWASP's
//! minimum: 19 MiB, 2 passes, 1 lane), kept as a self-describing string, and checked against a
//! small policy when one is chosen.

use anyhow::{Result, anyhow};
use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};

/// The shortest password the policy can be set to allow.
pub const MIN_LENGTH_FLOOR: usize = 8;

/// A new hash (`$argon2id$...`) with a random salt.
pub fn hash(password: &str) -> Result<String> {
    let mut salt = [0u8; 16];
    getrandom::fill(&mut salt).map_err(|e| anyhow!("the operating system has no random numbers: {e}"))?;
    let salt = SaltString::encode_b64(&salt).map_err(|e| anyhow!("salt: {e}"))?;
    let hash = Argon2::default().hash_password(password.as_bytes(), &salt).map_err(|e| anyhow!("hashing: {e}"))?;
    Ok(hash.to_string())
}

/// A random password for an account nobody will type in by hand (a dev database's first admin):
/// 24 letters and digits, about 120 bits.
pub fn generate() -> String {
    const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
    let mut bytes = [0u8; 24];
    getrandom::fill(&mut bytes).expect("OS random number generator");
    // 31 symbols: reject the top of the byte range so every symbol is equally likely.
    let limit = 255 - (256 % ALPHABET.len()) as u8;
    let mut out = String::with_capacity(24);
    let mut extra = [0u8; 1];
    for b in bytes {
        let mut b = b;
        while b > limit {
            getrandom::fill(&mut extra).expect("OS random number generator");
            b = extra[0];
        }
        out.push(ALPHABET[b as usize % ALPHABET.len()] as char);
    }
    out
}

/// Does `password` match `stored` (a string made by [`hash`])? A stored value that can't be read
/// matches nothing.
pub fn verify(password: &str, stored: &str) -> bool {
    match PasswordHash::new(stored) {
        Ok(parsed) => Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok(),
        Err(_) => false,
    }
}

/// Spend the time a real check takes, for a sign-in that names nobody: otherwise a fast refusal
/// tells a stranger which usernames exist.
pub fn waste_time(password: &str) {
    static DUMMY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let dummy = DUMMY.get_or_init(|| hash("not-a-real-account").unwrap_or_default());
    let _ = verify(password, dummy);
}

/// Passwords nobody should have even at the minimum length (lower case). Short on purpose: the
/// length rule removes most of the usual lists; these are the ones that get past it.
const COMMON: &[&str] = &[
    "password1234",
    "password12345",
    "passwordpassword",
    "123456789012",
    "1234567890123",
    "qwertyuiop12",
    "qwertyuiopasdf",
    "iloveyou1234",
    "administrator",
    "changemechangeme",
    "welcome12345",
    "letmeinletmein",
    "ductusductus",
    "hospital1234",
    "healthcare123",
];

/// Why `password` isn't acceptable for `username`, in words for the person choosing it.
pub fn check_policy(username: &str, password: &str, min_length: usize) -> std::result::Result<(), String> {
    let min_length = min_length.max(MIN_LENGTH_FLOOR);
    if password.chars().count() < min_length {
        return Err(format!("use at least {min_length} characters (a few ordinary words together work well)"));
    }
    let lower = password.to_lowercase();
    if !username.is_empty() && lower.contains(&username.to_lowercase()) {
        return Err("the password can't contain the username".into());
    }
    let first = password.chars().next();
    if password.chars().skip(1).all(|c| Some(c) == first) {
        return Err("the password can't be one character repeated".into());
    }
    if COMMON.contains(&lower.as_str()) {
        return Err("that password is one of the most common ones: pick another".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_password_checks_against_its_hash_only() {
        let stored = hash("correct horse battery").unwrap();
        assert!(stored.starts_with("$argon2id$"), "{stored}");
        assert!(verify("correct horse battery", &stored));
        assert!(!verify("correct horse batterx", &stored));
        assert!(!verify("correct horse battery", "not a hash"));
        assert_ne!(stored, hash("correct horse battery").unwrap(), "each hash has its own salt");
    }

    #[test]
    fn generated_passwords_pass_the_policy_and_differ() {
        let (a, b) = (generate(), generate());
        assert_eq!((a.len(), b.len()), (24, 24));
        assert_ne!(a, b);
        assert!(check_policy("dev", &a, 12).is_ok());
    }

    #[test]
    fn the_policy_explains_itself() {
        let ok = |u, p| check_policy(u, p, 12);
        assert!(ok("anna", "tulip river lantern").is_ok());
        assert!(ok("anna", "short").unwrap_err().contains("at least 12"));
        assert!(ok("anna", "xxANNAxxxxxxxxxx").unwrap_err().contains("username"));
        assert!(ok("anna", "aaaaaaaaaaaaaa").unwrap_err().contains("repeated"));
        assert!(ok("anna", "Password12345").unwrap_err().contains("common"));
        assert!(check_policy("anna", "1234567", 4).unwrap_err().contains("at least 8"), "the floor holds");
    }
}
