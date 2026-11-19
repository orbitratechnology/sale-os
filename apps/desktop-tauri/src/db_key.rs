//! Where the database key lives (ADR-023): the operating system's keychain (Windows Credential
//! Manager, protected by the Windows account), one entry per data folder.
//!
//! The first start generates a key, stores it, and reads it back before using it: a database
//! is never encrypted with a key that could not be read again. If the keychain cannot be used,
//! the till runs unencrypted and the Backups page says why.

use pos_local_db::crypto::{self, DbKey, FileKind};
use std::path::Path;
use std::sync::Mutex;

#[async_trait::async_trait]
pub trait DbKeyStore: Send + Sync {
    async fn get(&self) -> Result<Option<DbKey>, String>;
    async fn set(&self, key: &DbKey) -> Result<(), String>;
}

/// The OS keychain.
pub struct KeychainDbKey {
    account: String,
}

const SERVICE: &str = "sale-os.database";

impl KeychainDbKey {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            account: format!("pos.db@{}", data_dir.display()),
        }
    }

    async fn run<T: Send + 'static>(
        &self,
        f: impl FnOnce(keyring::Entry) -> Result<T, keyring::Error> + Send + 'static,
    ) -> Result<T, String> {
        let account = self.account.clone();
        tokio::task::spawn_blocking(move || {
            let entry = keyring::Entry::new(SERVICE, &account).map_err(|e| e.to_string())?;
            f(entry).map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
    }
}

#[async_trait::async_trait]
impl DbKeyStore for KeychainDbKey {
    async fn get(&self) -> Result<Option<DbKey>, String> {
        let text = self
            .run(|e| match e.get_password() {
                Ok(p) => Ok(Some(p)),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(other) => Err(other),
            })
            .await?;
        text.map(|t| DbKey::from_recovery_key(&t).map_err(|e| e.to_string()))
            .transpose()
    }

    async fn set(&self, key: &DbKey) -> Result<(), String> {
        let text = key.recovery_key();
        self.run(move |e| e.set_password(&text)).await
    }
}

/// In memory (tests).
#[derive(Default)]
pub struct MemoryDbKey(pub Mutex<Option<DbKey>>);

#[async_trait::async_trait]
impl DbKeyStore for MemoryDbKey {
    async fn get(&self) -> Result<Option<DbKey>, String> {
        Ok(self.0.lock().map_err(|e| e.to_string())?.clone())
    }
    async fn set(&self, key: &DbKey) -> Result<(), String> {
        *self.0.lock().map_err(|e| e.to_string())? = Some(key.clone());
        Ok(())
    }
}

/// No keychain at all: the database stays plain (tests and tools).
pub struct NoDbKey;

#[async_trait::async_trait]
impl DbKeyStore for NoDbKey {
    async fn get(&self) -> Result<Option<DbKey>, String> {
        Ok(None)
    }
    async fn set(&self, _key: &DbKey) -> Result<(), String> {
        Err("no keychain".into())
    }
}

/// Whether this till's data is encrypted, for the Backups page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Encryption {
    On,
    /// Running unencrypted, and why.
    Off(String),
}

/// The key to open the database with at start-up: the stored one; or, for a database that
/// is not encrypted yet (or not there yet), a new one, stored and read back first. `None` with
/// the reason when the keychain cannot be used. An encrypted database without a key stays
/// locked (the recovery-key screen).
pub async fn key_for_start(store: &dyn DbKeyStore, db_path: &Path) -> (Option<DbKey>, Encryption) {
    match store.get().await {
        Ok(Some(k)) => return (Some(k), Encryption::On),
        Ok(None) => {}
        Err(e) => {
            tracing::error!(error = %e, "the keychain could not be read");
            return (
                None,
                Encryption::Off(format!("the keychain could not be read: {e}")),
            );
        }
    }
    if crypto::file_kind(db_path) == FileKind::Encrypted {
        // Encrypted, and this computer has no key: needs the owner's recovery key.
        return (None, Encryption::On);
    }
    let key = match DbKey::generate() {
        Ok(k) => k,
        Err(e) => return (None, Encryption::Off(e.to_string())),
    };
    if let Err(e) = store.set(&key).await {
        tracing::warn!(error = %e, "no keychain: the database stays unencrypted");
        return (
            None,
            Encryption::Off(format!("the keychain is not available: {e}")),
        );
    }
    match store.get().await {
        Ok(Some(back)) if back == key => (Some(key), Encryption::On),
        other => {
            tracing::error!(read_back = ?other.map(|k| k.is_some()), "the stored key did not read back");
            (
                None,
                Encryption::Off("the key could not be read back from the keychain".into()),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_key_is_made_once_and_never_used_unless_it_reads_back() {
        let dir = tempfile::TempDir::new().unwrap();
        let db = dir.path().join("pos.db");
        let store = MemoryDbKey::default();
        let (first, on) = key_for_start(&store, &db).await;
        assert_eq!(on, Encryption::On);
        let (again, _) = key_for_start(&store, &db).await;
        assert_eq!(first, again, "the same key on the next start");

        let (none, off) = key_for_start(&NoDbKey, &db).await;
        assert!(none.is_none());
        assert!(matches!(off, Encryption::Off(_)));
    }
}
