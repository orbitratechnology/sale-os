use crate::IpcError;
use pos_local_db::LocalDb;

/// Process-wide services. Grows into the `AppServices` container (E01-T04).
pub struct AppState {
    pub db: Result<LocalDb, IpcError>,
}

impl AppState {
    pub fn new(db: Result<LocalDb, IpcError>) -> Self {
        Self { db }
    }
}
