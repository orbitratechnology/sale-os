//! Who an owner is (D-09): email and password for the pilot, behind `IdentityProvider` so a
//! managed OIDC provider can replace it before general availability.

use super::password::Hasher;
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    /// Wrong email or wrong password: deliberately one case.
    #[error("the email or the password is wrong")]
    InvalidCredentials,
    #[error("an account with this email already exists")]
    EmailTaken,
    #[error("{0}")]
    Invalid(&'static str),
    #[error("identity store: {0}")]
    Store(String),
}

#[async_trait::async_trait]
pub trait IdentityProvider: Send + Sync {
    async fn signup(&self, email: &str, password: &str) -> Result<Uuid, IdentityError>;
    async fn login(&self, email: &str, password: &str) -> Result<Uuid, IdentityError>;
}

/// Email and password accounts in `accounts`, hashed with Argon2id.
pub struct PasswordIdentity {
    pool: PgPool,
    hasher: Arc<Hasher>,
    /// A hash of a random password with the same cost: an unknown email is verified against
    /// it, so it takes as long as a wrong password and the two cannot be told apart.
    dummy: String,
}

impl PasswordIdentity {
    pub fn new(pool: PgPool, hasher: Arc<Hasher>) -> Self {
        let random: String = {
            use argon2::password_hash::rand_core::{OsRng, RngCore};
            let mut b = [0u8; 24];
            OsRng.fill_bytes(&mut b);
            b.iter().map(|x| format!("{x:02x}")).collect()
        };
        // Even if guessed it signs no one in: it is only used when there is no account.
        let dummy = hasher.hash(&random);
        Self { pool, hasher, dummy }
    }
}

/// Trimmed, and plausibly an email address.
pub fn clean_email(email: &str) -> Result<String, IdentityError> {
    let e = email.trim();
    let ok = e.len() <= 254
        && !e.chars().any(char::is_whitespace)
        && e.split_once('@').is_some_and(|(u, h)| {
            !u.is_empty() && h.contains('.') && !h.starts_with('.') && !h.ends_with('.')
        });
    if ok {
        Ok(e.to_owned())
    } else {
        Err(IdentityError::Invalid("that is not an email address"))
    }
}

#[async_trait::async_trait]
impl IdentityProvider for PasswordIdentity {
    async fn signup(&self, email: &str, password: &str) -> Result<Uuid, IdentityError> {
        let email = clean_email(email)?;
        super::password::check(password).map_err(IdentityError::Invalid)?;
        let hasher = self.hasher.clone();
        let password = password.to_owned();
        let hash = tokio::task::spawn_blocking(move || hasher.hash(&password))
            .await
            .map_err(|e| IdentityError::Store(e.to_string()))?;
        let id = Uuid::now_v7();
        pos_cloud_db::accounts::create(&self.pool, id, &email, &hash)
            .await
            .map_err(|e| match e {
                pos_cloud_db::accounts::AccountError::EmailTaken => IdentityError::EmailTaken,
                other => IdentityError::Store(other.to_string()),
            })?;
        Ok(id)
    }

    async fn login(&self, email: &str, password: &str) -> Result<Uuid, IdentityError> {
        let row = match clean_email(email) {
            Ok(e) => pos_cloud_db::accounts::by_email(&self.pool, &e)
                .await
                .map_err(|e| IdentityError::Store(e.to_string()))?,
            Err(_) => None,
        };
        let usable = row.filter(|r| r.status == "active");
        let hash = usable
            .as_ref()
            .and_then(|r| r.password_hash.clone())
            .unwrap_or_else(|| self.dummy.clone());
        let hasher = self.hasher.clone();
        let password = password.to_owned();
        let matches = tokio::task::spawn_blocking(move || hasher.verify(&hash, &password))
            .await
            .map_err(|e| IdentityError::Store(e.to_string()))?;
        match usable {
            Some(r) if matches && r.password_hash.is_some() => Ok(r.account_id),
            _ => Err(IdentityError::InvalidCredentials),
        }
    }
}
