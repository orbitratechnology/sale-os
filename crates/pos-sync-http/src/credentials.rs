//! Where the device's refresh token lives (ADR-013): the OS keychain (Keychain on macOS,
//! Credential Manager on Windows, the Secret Service on Linux). Only the refresh token; the
//! access token stays in memory.

use pos_application::link::{CredentialStore, LinkError};
use pos_domain::Secret;
use std::sync::Mutex;

/// In memory only: tests, and a session that should forget on exit.
#[derive(Default)]
pub struct MemoryCredentials(Mutex<Option<Secret<String>>>);

#[async_trait::async_trait]
impl CredentialStore for MemoryCredentials {
    async fn get_refresh_token(&self) -> Result<Option<Secret<String>>, LinkError> {
        Ok(self
            .0
            .lock()
            .map_err(|_| LinkError::Credentials("poisoned".into()))?
            .clone())
    }
    async fn set_refresh_token(&self, token: Secret<String>) -> Result<(), LinkError> {
        *self
            .0
            .lock()
            .map_err(|_| LinkError::Credentials("poisoned".into()))? = Some(token);
        Ok(())
    }
    async fn clear(&self) -> Result<(), LinkError> {
        *self
            .0
            .lock()
            .map_err(|_| LinkError::Credentials("poisoned".into()))? = None;
        Ok(())
    }
}

/// The OS keychain entry `service` / `account` (one per installation).
pub struct KeychainCredentials {
    pub service: String,
    pub account: String,
}

impl KeychainCredentials {
    pub fn new(installation_id: &str) -> Self {
        Self {
            service: "sale-os.device".into(),
            account: installation_id.into(),
        }
    }

    async fn run<T: Send + 'static>(
        &self,
        f: impl FnOnce(keyring::Entry) -> Result<T, keyring::Error> + Send + 'static,
    ) -> Result<T, LinkError> {
        let (service, account) = (self.service.clone(), self.account.clone());
        // The platform APIs block (and the Secret Service client runs its own runtime).
        tokio::task::spawn_blocking(move || {
            let entry =
                keyring::Entry::new(&service, &account).map_err(|e| LinkError::Credentials(e.to_string()))?;
            f(entry).map_err(|e| LinkError::Credentials(e.to_string()))
        })
        .await
        .map_err(|e| LinkError::Credentials(e.to_string()))?
    }
}

#[async_trait::async_trait]
impl CredentialStore for KeychainCredentials {
    async fn get_refresh_token(&self) -> Result<Option<Secret<String>>, LinkError> {
        self.run(|e| match e.get_password() {
            Ok(p) => Ok(Some(Secret::new(p))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(other) => Err(other),
        })
        .await
    }
    async fn set_refresh_token(&self, token: Secret<String>) -> Result<(), LinkError> {
        self.run(move |e| e.set_password(token.expose())).await
    }
    async fn clear(&self) -> Result<(), LinkError> {
        self.run(|e| match e.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(other) => Err(other),
        })
        .await
    }
}
