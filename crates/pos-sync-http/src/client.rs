//! The cloud API over HTTPS.

use pos_application::link::{
    CloudAccount, CloudBusiness, DeviceRegistration, LinkError, NewCloudBusiness, OwnerSession, Registered,
};
use pos_application::sync::TransportError;
use pos_domain::Secret;
use pos_sync::pull::{BootstrapPage, ChangesResponse};
use pos_sync::reports::{GroupBy, SalesReport};
use pos_sync::{PushRequest, PushResponse};
use reqwest::{Method, RequestBuilder, Response, StatusCode};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::time::Duration;
use uuid::Uuid;

/// One cloud, at `base_url` (e.g. `https://api.example.com`).
#[derive(Clone)]
pub struct CloudClient {
    http: reqwest::Client,
    base: String,
    app_version: String,
}

/// A refresh's result. Never logged.
pub struct DeviceTokens {
    pub access_token: Secret<String>,
    pub refresh_token: Secret<String>,
    pub expires_in: i64,
}

/// A failed call: the status and the problem's `code`, or no answer at all.
#[derive(Debug)]
enum Failure {
    Network {
        timeout: bool,
    },
    Status {
        status: StatusCode,
        code: Option<String>,
        min_version: Option<String>,
    },
    Decode(String),
}

impl CloudClient {
    pub fn new(base_url: &str, app_version: &str) -> Result<Self, String> {
        // TLS with the `ring` provider and the operating system's certificate store.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            http,
            base: base_url.trim_end_matches('/').to_owned(),
            app_version: app_version.to_owned(),
        })
    }

    fn request(&self, method: Method, path: &str, token: Option<&Secret<String>>) -> RequestBuilder {
        let mut r = self
            .http
            .request(method, format!("{}{path}", self.base))
            .header("x-app-version", &self.app_version);
        if let Some(t) = token {
            r = r.bearer_auth(t.expose());
        }
        r
    }

    async fn send<T: DeserializeOwned>(&self, r: RequestBuilder) -> Result<(StatusCode, T), Failure> {
        let res: Response = r.send().await.map_err(|e| Failure::Network {
            timeout: e.is_timeout(),
        })?;
        let status = res.status();
        let bytes = res.bytes().await.map_err(|e| Failure::Network {
            timeout: e.is_timeout(),
        })?;
        if status.is_success() {
            let body = if bytes.is_empty() {
                b"null".as_slice()
            } else {
                &bytes
            };
            return serde_json::from_slice(body)
                .map(|v| (status, v))
                .map_err(|e| Failure::Decode(e.to_string()));
        }
        let problem: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        Err(Failure::Status {
            status,
            code: problem["code"].as_str().map(str::to_owned),
            min_version: problem["details"]["minVersion"].as_str().map(str::to_owned),
        })
    }

    // ---------- the device ----------

    pub async fn push(
        &self,
        token: &Secret<String>,
        req: &PushRequest,
    ) -> Result<PushResponse, TransportError> {
        let r = self
            .request(Method::POST, "/v1/sync/events", Some(token))
            .json(req);
        self.send(r).await.map(|(_, v)| v).map_err(transport)
    }

    pub async fn changes(
        &self,
        token: &Secret<String>,
        cursor: Option<&str>,
        limit: u32,
        unsent: Option<u64>,
    ) -> Result<ChangesResponse, TransportError> {
        let mut r = self
            .request(Method::GET, "/v1/sync/changes", Some(token))
            .query(&[("limit", limit)]);
        if let Some(c) = cursor {
            r = r.query(&[("cursor", c)]);
        }
        if let Some(n) = unsent {
            r = r.query(&[("unsent", n)]);
        }
        self.send(r).await.map(|(_, v)| v).map_err(transport)
    }

    /// `GET /v1/reports/sales` with the device's token (its own business).
    pub async fn sales_report(
        &self,
        token: &Secret<String>,
        from: &str,
        to: &str,
        group_by: GroupBy,
    ) -> Result<SalesReport, TransportError> {
        let by = match group_by {
            GroupBy::Day => "day",
            GroupBy::Register => "register",
            GroupBy::Tender => "tender",
        };
        let r = self
            .request(Method::GET, "/v1/reports/sales", Some(token))
            .query(&[("from", from), ("to", to), ("groupBy", by)]);
        self.send(r).await.map(|(_, v)| v).map_err(transport)
    }

    pub async fn bootstrap(
        &self,
        token: &Secret<String>,
        page: Option<&str>,
    ) -> Result<BootstrapPage, TransportError> {
        let mut r = self.request(Method::GET, "/v1/sync/bootstrap", Some(token));
        if let Some(p) = page {
            r = r.query(&[("page", p)]);
        }
        self.send(r).await.map(|(_, v)| v).map_err(transport)
    }

    /// Rotates the refresh token (the old one is spent the moment the cloud answers).
    pub async fn refresh(&self, refresh_token: &Secret<String>) -> Result<DeviceTokens, TransportError> {
        let r = self
            .request(Method::POST, "/v1/auth/device/refresh", None)
            .json(&json!({"refreshToken": refresh_token.expose()}));
        let (_, v): (_, Value) = self.send(r).await.map_err(|f| match f {
            // An unknown or expired refresh token cannot recover either: link again.
            Failure::Status {
                status: StatusCode::UNAUTHORIZED,
                ..
            } => TransportError::DeviceRevoked,
            other => transport(other),
        })?;
        tokens(&v).ok_or_else(|| TransportError::Decode("refresh reply".into()))
    }
}

fn tokens(v: &Value) -> Option<DeviceTokens> {
    Some(DeviceTokens {
        access_token: Secret::new(v["accessToken"].as_str()?.to_owned()),
        refresh_token: Secret::new(v["refreshToken"].as_str()?.to_owned()),
        expires_in: v["expiresIn"].as_i64()?,
    })
}

/// How a failed device call reads to the sync worker.
fn transport(f: Failure) -> TransportError {
    match f {
        Failure::Network { timeout: true } => TransportError::Timeout,
        Failure::Network { timeout: false } => TransportError::Offline,
        Failure::Decode(d) => TransportError::Decode(d),
        Failure::Status {
            status,
            code,
            min_version,
        } => match (status.as_u16(), code.as_deref()) {
            (401, Some("SYNC_DEVICE_REVOKED")) => TransportError::DeviceRevoked,
            (401, _) => TransportError::Unauthorized,
            (426, _) => TransportError::UpgradeRequired {
                min_version: min_version.unwrap_or_default(),
            },
            (s, _) => TransportError::Server { status: s, code },
        },
    }
}

/// How a failed owner call reads to the link wizard.
fn link(f: Failure) -> LinkError {
    match f {
        Failure::Network { .. } => LinkError::Offline,
        Failure::Decode(d) => LinkError::Other(format!("unreadable reply from the cloud: {d}")),
        Failure::Status { status, code, .. } => match (status.as_u16(), code.as_deref()) {
            (401, Some("AUTH_CREDENTIALS_INVALID")) => LinkError::CredentialsInvalid,
            (429, _) => LinkError::RateLimited,
            (409, Some("SYNC_BUSINESS_ID_TAKEN")) => LinkError::BusinessTaken,
            (409, Some("SYNC_REGISTER_IN_USE")) => LinkError::RegisterInUse,
            (404, _) => LinkError::NotFound,
            (426, _) => LinkError::UpgradeRequired,
            (s, _) if s >= 500 => LinkError::Offline,
            (s, c) => LinkError::Other(format!("the cloud answered {s} {}", c.unwrap_or(""))),
        },
    }
}

#[async_trait::async_trait]
impl CloudAccount for CloudClient {
    async fn login(&self, email: &str, password: &Secret<String>) -> Result<OwnerSession, LinkError> {
        let r = self
            .request(Method::POST, "/v1/auth/login", None)
            .json(&json!({"email": email, "password": password.expose()}));
        let (_, v): (_, Value) = self.send(r).await.map_err(link)?;
        let token = v["userAccessToken"]
            .as_str()
            .ok_or_else(|| LinkError::Other("no token in the sign-in reply".into()))?;
        Ok(OwnerSession {
            token: Secret::new(token.to_owned()),
        })
    }

    async fn businesses(&self, owner: &OwnerSession) -> Result<Vec<CloudBusiness>, LinkError> {
        let r = self.request(Method::GET, "/v1/businesses", Some(&owner.token));
        let (_, v): (_, Vec<Value>) = self.send(r).await.map_err(link)?;
        Ok(v.iter()
            .filter_map(|b| {
                Some(CloudBusiness {
                    business_id: b["businessId"].as_str()?.parse().ok()?,
                    name: b["name"].as_str()?.to_owned(),
                    currency_code: b["currencyCode"].as_str().unwrap_or_default().to_owned(),
                })
            })
            .collect())
    }

    async fn create_business(&self, owner: &OwnerSession, b: &NewCloudBusiness) -> Result<(), LinkError> {
        let r = self.request(Method::POST, "/v1/businesses", Some(&owner.token)).json(&json!({
            "businessId": b.business_id, "name": b.name, "countryCode": b.country_code, "timezone": b.timezone,
            "currencyCode": b.currency_code, "currencyExponent": b.currency_exponent,
            "pricesIncludeTax": b.prices_include_tax,
        }));
        self.send::<Value>(r).await.map(|_| ()).map_err(link)
    }

    async fn register_device(
        &self,
        owner: &OwnerSession,
        d: &DeviceRegistration,
    ) -> Result<Registered, LinkError> {
        let mut body = json!({
            "businessId": d.business_id, "registerId": d.register_id, "registerName": d.register_name,
            "installationId": d.installation_id, "appVersion": d.app_version, "osInfo": std::env::consts::OS,
            "replaceExisting": d.replace_existing,
        });
        if let Some(c) = &d.register_code {
            body["registerCode"] = json!(c);
        }
        if let Some((id, name)) = &d.location {
            body["locationId"] = json!(id);
            body["locationName"] = json!(name);
        }
        let r = self
            .request(Method::POST, "/v1/devices/register", Some(&owner.token))
            .json(&body);
        let (_, v): (_, Value) = self.send(r).await.map_err(link)?;
        let t =
            tokens(&v).ok_or_else(|| LinkError::Other("no credentials in the registration reply".into()))?;
        let uuid = |k: &str| v[k].as_str().and_then(|s| s.parse::<Uuid>().ok());
        Ok(Registered {
            device_id: uuid("deviceId").ok_or_else(|| LinkError::Other("no device id".into()))?,
            access_token: t.access_token,
            expires_in: t.expires_in,
            refresh_token: t.refresh_token,
            register_code: v["registerCode"].as_str().unwrap_or_default().to_owned(),
            location_id: uuid("locationId").unwrap_or_default(),
        })
    }
}
