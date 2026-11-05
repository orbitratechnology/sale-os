//! IPC command adapters (docs/backlog/07-contracts.md §1.3).

use crate::{AppState, IpcError};
use pos_domain::{Currency, Money, NumberFormat};
use serde::Serialize;
use tauri::State;

#[derive(Debug, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub version: String,
    pub debug_build: bool,
    pub database: DatabaseStatus,
}

#[derive(Debug, Serialize, specta::Type)]
#[serde(tag = "state", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum DatabaseStatus {
    Ready { path: String, schema_version: u32 },
    Failed { error: IpcError },
}

/// Version, build flavour and local database status. No session required.
#[tauri::command]
#[specta::specta]
pub async fn app_info(state: State<'_, AppState>) -> Result<AppInfo, IpcError> {
    let database = match &state.db {
        Ok(db) => {
            let v = db.schema_version().await.map_err(|e| IpcError::from_error(&e))?;
            DatabaseStatus::Ready {
                path: db.path().display().to_string(),
                schema_version: u32::try_from(v).map_err(IpcError::internal)?,
            }
        }
        Err(e) => DatabaseStatus::Failed { error: e.clone() },
    };
    Ok(AppInfo {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        debug_build: cfg!(debug_assertions),
        database,
    })
}

#[derive(Debug, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct MoneyAddResult {
    /// Decimal string of minor units (ADR-001): never a JS number.
    pub sum_minor: String,
    pub formatted: String,
}

/// Stage 0 demo: adds two amounts with `pos-domain` (the same crate the cloud uses) and
/// formats the result. Money crosses IPC as minor-unit strings.
#[tauri::command]
#[specta::specta]
pub fn dev_money_add(a_minor: String, b_minor: String, currency: String) -> Result<MoneyAddResult, IpcError> {
    let cur = Currency::from_iso(&currency).map_err(|e| IpcError::from_error(&e))?;
    let parse = |s: &str| {
        s.parse::<i64>()
            .map(Money::from_minor)
            .map_err(|_| IpcError::from_error(&InvalidMinor(s.to_owned())))
    };
    let sum = parse(&a_minor)?
        .checked_add(parse(&b_minor)?)
        .map_err(|e| IpcError::from_error(&e))?;
    Ok(MoneyAddResult {
        sum_minor: sum.minor().to_string(),
        formatted: cur.format(sum, NumberFormat::EN_US),
    })
}

#[derive(Debug)]
struct InvalidMinor(String);

impl std::fmt::Display for InvalidMinor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "not a minor-unit integer: {:?}", self.0)
    }
}

impl pos_domain::HasErrorCode for InvalidMinor {
    fn error_code(&self) -> pos_domain::ErrorCode {
        pos_domain::ErrorCode::ValidationFailed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_add_formats_with_currency_exponent() {
        let r = dev_money_add("1999".into(), "1".into(), "USD".into()).unwrap();
        assert_eq!((r.sum_minor.as_str(), r.formatted.as_str()), ("2000", "20.00"));
        let r = dev_money_add("1500".into(), "0".into(), "KWD".into()).unwrap();
        assert_eq!(r.formatted, "1.500");
    }

    #[test]
    fn money_add_errors_are_catalogued() {
        let e = dev_money_add("1.5".into(), "1".into(), "USD".into()).unwrap_err();
        assert_eq!(e.code, pos_domain::ErrorCode::ValidationFailed);
        let e = dev_money_add(i64::MAX.to_string(), "1".into(), "USD".into()).unwrap_err();
        assert_eq!(e.code, pos_domain::ErrorCode::ValidationFailed);
    }
}
