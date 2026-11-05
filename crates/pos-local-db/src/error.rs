use pos_domain::{ErrorCode, HasErrorCode};
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(
        "the database is on a network path ({0}); keep it on this computer's local disk — \
         each register owns its own database and synchronises through the cloud"
    )]
    NetworkPathRefused(PathBuf),
    #[error(
        "database schema version {db_version} is newer than this app supports ({app_version}); update the app"
    )]
    SchemaTooNew { db_version: i64, app_version: i64 },
    #[error("migration failed: {0}")]
    Migration(String),
    #[error("database is busy")]
    Busy,
    #[error("database integrity problem: {0}")]
    Corrupt(String),
    #[error("{context}: {message}")]
    Io { context: &'static str, message: String },
}

impl StoreError {
    pub(crate) fn io(context: &'static str, e: impl std::fmt::Display) -> Self {
        StoreError::Io {
            context,
            message: e.to_string(),
        }
    }

    pub(crate) fn from_sqlx(e: sqlx::Error) -> Self {
        if let sqlx::Error::Database(db) = &e {
            // SQLITE_BUSY (5) / SQLITE_LOCKED (6), including extended codes.
            if let Some(code) = db.code().and_then(|c| c.parse::<i32>().ok()) {
                match code & 0xff {
                    5 | 6 => return StoreError::Busy,
                    11 | 26 => return StoreError::Corrupt(db.message().to_owned()),
                    _ => {}
                }
            }
        }
        StoreError::io("sqlite", e)
    }
}

impl HasErrorCode for StoreError {
    fn error_code(&self) -> ErrorCode {
        match self {
            StoreError::NetworkPathRefused(_) | StoreError::Io { .. } | StoreError::Migration(_) => {
                ErrorCode::StoreIo
            }
            StoreError::SchemaTooNew { .. } => ErrorCode::StoreSchemaTooNew,
            StoreError::Busy => ErrorCode::StoreBusy,
            StoreError::Corrupt(_) => ErrorCode::StoreCorrupt,
        }
    }
}
