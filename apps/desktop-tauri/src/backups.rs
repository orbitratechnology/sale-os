//! Automatic local backups (E19-T03, ADR-007): a daily copy, an hourly one when anything
//! changed since the last backup, and one when the app closes; each also copied to the
//! owner's second folder (a USB stick or external drive) when one is set.
//!
//! The second folder is remembered in `backup-config.json` next to the database, outside it,
//! so it is known even when the database is the thing that is damaged.

use pos_local_db::LocalDb;
use pos_local_db::backup::{self, BackupInfo, BackupKind};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const HOUR_MS: i64 = 3_600_000;
const DAY_MS: i64 = 24 * HOUR_MS;
/// How often the worker looks.
const TICK: Duration = Duration::from_secs(300);

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Config {
    secondary_folder: Option<PathBuf>,
    /// When the owner confirmed writing the recovery key down (ADR-023), epoch ms.
    #[serde(default)]
    recovery_key_confirmed_at: Option<i64>,
}

/// The last copy to the second folder, for the settings page.
#[derive(Clone, Debug, Default)]
pub(crate) struct SecondaryState {
    pub last_ok_at: Option<i64>,
    pub problem: Option<String>,
}

pub(crate) struct Backups {
    db: LocalDb,
    config_path: PathBuf,
    secondary: Mutex<SecondaryState>,
    /// One backup at a time (the worker, "Back up now" and shutdown never overlap).
    running: tokio::sync::Mutex<()>,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// What the worker should do now, given the backups there are, the time and the last outbox
/// sequence: a daily copy once a day; otherwise an hourly one if an hour has passed since the
/// newest backup and something was written since.
pub(crate) fn due(backups: &[BackupInfo], now: i64, outbox_sequence: i64) -> Option<BackupKind> {
    let newest = |kinds: &[BackupKind]| {
        backups
            .iter()
            .filter(|b| kinds.contains(&b.meta.kind))
            .max_by_key(|b| b.meta.created_at)
    };
    if newest(&[BackupKind::Daily]).is_none_or(|b| now - b.meta.created_at >= DAY_MS) {
        return Some(BackupKind::Daily);
    }
    let last = newest(&[
        BackupKind::Daily,
        BackupKind::Hourly,
        BackupKind::Shutdown,
        BackupKind::Manual,
    ])?;
    (now - last.meta.created_at >= HOUR_MS && outbox_sequence != last.meta.outbox_sequence)
        .then_some(BackupKind::Hourly)
}

impl Backups {
    pub(crate) fn new(db: LocalDb, data_dir: &Path) -> Arc<Self> {
        Arc::new(Self {
            db,
            config_path: data_dir.join("backup-config.json"),
            secondary: Mutex::new(SecondaryState::default()),
            running: tokio::sync::Mutex::new(()),
        })
    }

    /// Starts the worker on the current runtime.
    pub(crate) fn start(self: &Arc<Self>) {
        let me = self.clone();
        tokio::spawn(async move {
            // The first look a minute after start-up, not during it.
            tokio::time::sleep(Duration::from_secs(60)).await;
            loop {
                if let Err(e) = me.tick().await {
                    tracing::warn!(error = %e, "automatic backup failed");
                }
                tokio::time::sleep(TICK).await;
            }
        });
    }

    async fn tick(&self) -> Result<(), pos_local_db::StoreError> {
        let (outbox_sequence, _) = self.db.change_marker().await?;
        if let Some(kind) = due(&self.db.backups(), now_ms(), outbox_sequence) {
            self.make(kind).await?;
        }
        Ok(())
    }

    /// Makes a backup now, and copies it to the second folder if there is one. A failed copy
    /// is reported on the settings page; the backup itself still stands.
    pub(crate) async fn make(&self, kind: BackupKind) -> Result<BackupInfo, pos_local_db::StoreError> {
        let _one = self.running.lock().await;
        let made = self.db.backup(kind).await?;
        if let Some(folder) = self.secondary_folder() {
            let result = backup::copy_to(&made, &folder);
            let mut s = self
                .secondary
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match result {
                Ok(_) => {
                    s.last_ok_at = Some(made.meta.created_at);
                    s.problem = None;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "copy to the second backup folder failed");
                    s.problem = Some(e.to_string());
                }
            }
        }
        Ok(made)
    }

    pub(crate) fn db(&self) -> &LocalDb {
        &self.db
    }

    fn config(&self) -> Config {
        std::fs::read_to_string(&self.config_path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub(crate) fn secondary_folder(&self) -> Option<PathBuf> {
        self.config().secondary_folder
    }

    pub(crate) fn secondary_state(&self) -> SecondaryState {
        self.secondary
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Sets (or with `None` clears) the second folder.
    pub(crate) fn set_secondary_folder(&self, folder: Option<PathBuf>) -> std::io::Result<()> {
        let mut config = self.config();
        config.secondary_folder = folder;
        self.save(&config)?;
        *self
            .secondary
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = SecondaryState::default();
        Ok(())
    }

    fn save(&self, config: &Config) -> std::io::Result<()> {
        std::fs::write(
            &self.config_path,
            serde_json::to_string_pretty(config).map_err(std::io::Error::other)?,
        )
    }

    pub(crate) fn recovery_key_confirmed(&self) -> bool {
        self.config().recovery_key_confirmed_at.is_some()
    }

    pub(crate) fn confirm_recovery_key(&self) -> std::io::Result<()> {
        let mut config = self.config();
        config.recovery_key_confirmed_at = Some(now_ms());
        self.save(&config)
    }

    /// The backups on this computer and in the second folder, newest first.
    pub(crate) fn all(&self) -> Vec<(BackupInfo, bool)> {
        let mut out: Vec<(BackupInfo, bool)> = self.db.backups().into_iter().map(|b| (b, false)).collect();
        if let Some(folder) = self.secondary_folder() {
            let here: Vec<String> = out.iter().map(|(b, _)| b.id.clone()).collect();
            out.extend(
                backup::list(&folder)
                    .into_iter()
                    .filter(|b| !here.contains(&b.id))
                    .map(|b| (b, true)),
            );
        }
        out.sort_by_key(|b| std::cmp::Reverse(b.0.meta.created_at));
        out
    }

    /// Finds a backup by the id the settings page showed.
    pub(crate) fn find(&self, id: &str) -> Option<BackupInfo> {
        self.all().into_iter().map(|(b, _)| b).find(|b| b.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pos_local_db::backup::BackupMeta;

    fn b(kind: BackupKind, created_at: i64, outbox_sequence: i64) -> BackupInfo {
        BackupInfo {
            id: String::new(),
            path: PathBuf::new(),
            size_bytes: 0,
            meta: BackupMeta {
                kind,
                created_at,
                schema_version: 1,
                outbox_sequence,
                sales: 0,
                app_version: String::new(),
            },
        }
    }

    #[test]
    fn daily_first_then_hourly_only_when_something_changed() {
        let now = 100 * DAY_MS;
        assert_eq!(due(&[], now, 0), Some(BackupKind::Daily));
        let today = [b(BackupKind::Daily, now - 2 * HOUR_MS, 40)];
        assert_eq!(due(&today, now, 40), None, "nothing written since");
        assert_eq!(due(&today, now, 41), Some(BackupKind::Hourly));
        let recent = [
            b(BackupKind::Daily, now - 3 * HOUR_MS, 40),
            b(BackupKind::Hourly, now - 10 * 60_000, 40),
        ];
        assert_eq!(
            due(&recent, now, 45),
            None,
            "the last backup is under an hour old"
        );
        let yesterday = [b(BackupKind::Daily, now - DAY_MS, 40)];
        assert_eq!(due(&yesterday, now, 40), Some(BackupKind::Daily));
    }
}
