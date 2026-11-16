//! The sync transport (E17-T03/T07 ports) for a linked register.

use crate::client::CloudClient;
use pos_application::link::{CredentialStore, SessionSeed};
use pos_application::sync::{SyncTransport, TransportError};
use pos_domain::Secret;
use pos_sync::pull::{BootstrapPage, ChangesResponse};
use pos_sync::{PushRequest, PushResponse};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Refresh this long before the access token expires.
const MARGIN: Duration = Duration::from_secs(60);

/// A linked register's connection: the cached device token, refreshed with the refresh
/// token from the credential store when it is about to expire or is refused.
pub struct DeviceSession {
    client: CloudClient,
    credentials: Arc<dyn CredentialStore>,
    access: Mutex<Option<(Secret<String>, Instant)>>,
    /// One refresh at a time: two concurrent rotations of one refresh token would look like
    /// reuse to the cloud, which revokes the device.
    refreshing: tokio::sync::Mutex<()>,
}

impl DeviceSession {
    pub fn new(client: CloudClient, credentials: Arc<dyn CredentialStore>) -> Self {
        Self {
            client,
            credentials,
            access: Mutex::new(None),
            refreshing: tokio::sync::Mutex::new(()),
        }
    }

    fn cached(&self) -> Option<Secret<String>> {
        let guard = self.access.lock().ok()?;
        let (token, until) = guard.as_ref()?;
        (Instant::now() + MARGIN < *until).then(|| token.clone())
    }

    fn forget(&self) {
        if let Ok(mut g) = self.access.lock() {
            *g = None;
        }
    }

    /// A usable access token: the cached one, or a fresh one from a refresh.
    async fn token(&self) -> Result<Secret<String>, TransportError> {
        if let Some(t) = self.cached() {
            return Ok(t);
        }
        let _one = self.refreshing.lock().await;
        if let Some(t) = self.cached() {
            return Ok(t);
        }
        let refresh = self
            .credentials
            .get_refresh_token()
            .await
            .map_err(|_| TransportError::Unauthorized)?
            .ok_or(TransportError::Unauthorized)?;
        let fresh = self.client.refresh(&refresh).await?;
        // The old refresh token is spent now: keep the new one before anything else.
        if self
            .credentials
            .set_refresh_token(fresh.refresh_token)
            .await
            .is_err()
        {
            tracing::error!("could not store the rotated refresh token");
            return Err(TransportError::Unauthorized);
        }
        self.seed(fresh.access_token.clone(), fresh.expires_in);
        Ok(fresh.access_token)
    }

    /// Runs a call, refreshing once if the access token was refused.
    async fn call<T, F, Fut>(&self, f: F) -> Result<T, TransportError>
    where
        F: Fn(Secret<String>) -> Fut,
        Fut: std::future::Future<Output = Result<T, TransportError>>,
    {
        let token = self.token().await?;
        match f(token).await {
            Err(TransportError::Unauthorized) => {
                self.forget();
                f(self.token().await?).await
            }
            other => other,
        }
    }
}

impl SessionSeed for DeviceSession {
    fn seed(&self, access_token: Secret<String>, expires_in_secs: i64) {
        let ttl = Duration::from_secs(u64::try_from(expires_in_secs).unwrap_or(0));
        if let Ok(mut g) = self.access.lock() {
            *g = Some((access_token, Instant::now() + ttl));
        }
    }
}

#[async_trait::async_trait]
impl SyncTransport for DeviceSession {
    async fn push(&self, request: PushRequest) -> Result<PushResponse, TransportError> {
        self.call(|t| {
            let request = request.clone();
            async move { self.client.push(&t, &request).await }
        })
        .await
    }

    async fn changes(&self, cursor: Option<String>, limit: u32) -> Result<ChangesResponse, TransportError> {
        self.call(|t| {
            let cursor = cursor.clone();
            async move { self.client.changes(&t, cursor.as_deref(), limit).await }
        })
        .await
    }

    async fn bootstrap(&self, page: Option<String>) -> Result<BootstrapPage, TransportError> {
        self.call(|t| {
            let page = page.clone();
            async move { self.client.bootstrap(&t, page.as_deref()).await }
        })
        .await
    }
}
