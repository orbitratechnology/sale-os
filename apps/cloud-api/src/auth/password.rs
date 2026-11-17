//! Owner passwords (E16-T03): at least 10 characters, not one of the 10,000 most common
//! (SecLists, MIT; `data/common-passwords-10k.txt`), hashed with Argon2id (ADR-013).

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use std::collections::HashSet;
use std::sync::LazyLock;

pub const MIN_LEN: usize = 10;
pub const MAX_LEN: usize = 128;

static COMMON: LazyLock<HashSet<String>> = LazyLock::new(|| {
    include_str!("../../data/common-passwords-10k.txt")
        .lines()
        .map(|l| l.trim().to_lowercase())
        .filter(|l| !l.is_empty())
        .collect()
});

/// Why a password is refused, in words for the owner.
pub fn check(password: &str) -> Result<(), &'static str> {
    let n = password.chars().count();
    if n < MIN_LEN {
        return Err("use at least 10 characters");
    }
    if n > MAX_LEN {
        return Err("use at most 128 characters");
    }
    if COMMON.contains(&password.to_lowercase()) {
        return Err("this password is too common; choose another");
    }
    Ok(())
}

/// Argon2id with the configured cost. Hashing and verifying are deliberately slow; callers
/// run them on a blocking thread.
pub struct Hasher {
    argon: Argon2<'static>,
}

impl Hasher {
    /// `m_kib` memory, `t` passes (ADR-013: 64 MiB, 3).
    pub fn new(m_kib: u32, t: u32) -> Self {
        Self {
            argon: Argon2::new(
                Algorithm::Argon2id,
                Version::V0x13,
                Params::new(m_kib, t, 1, None).expect("valid argon2 params"),
            ),
        }
    }

    pub fn hash(&self, password: &str) -> String {
        let salt = SaltString::generate(&mut OsRng);
        self.argon
            .hash_password(password.as_bytes(), &salt)
            .expect("argon2 hashing")
            .to_string()
    }

    /// Whether `password` matches the PHC string. A malformed hash never matches.
    pub fn verify(&self, phc: &str, password: &str) -> bool {
        PasswordHash::new(phc).is_ok_and(|h| self.argon.verify_password(password.as_bytes(), &h).is_ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn length_and_common_passwords() {
        assert_eq!(check("short"), Err("use at least 10 characters"));
        assert!(check("1234567890").is_err(), "in the common list");
        assert!(check("Qwertyuiop").is_err(), "the list is compared without case");
        assert_eq!(check("correct horse battery staple"), Ok(()));
        assert!(check(&"x".repeat(129)).is_err());
    }

    #[test]
    fn hashes_verify_and_are_salted() {
        let h = Hasher::new(256, 1);
        let a = h.hash("correct horse battery staple");
        let b = h.hash("correct horse battery staple");
        assert_ne!(a, b);
        assert!(a.starts_with("$argon2id$v=19$m=256,t=1,p=1$"));
        assert!(h.verify(&a, "correct horse battery staple"));
        assert!(!h.verify(&a, "correct horse battery stapler"));
        assert!(!h.verify("not a hash", "anything"));
    }
}
