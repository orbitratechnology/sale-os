//! Database encryption at rest (ADR-023): SQLCipher 4 (AES-256, per-page HMAC), built into
//! `libsqlite3-sys` in place of plain SQLite. The app holds a random 256-bit key in the
//! operating system's keychain and passes it as a raw key (no passphrase stretching), so
//! opening costs nothing extra.
//!
//! Backups are made with `VACUUM INTO` on a keyed connection, so they are encrypted with the
//! same key, on this computer and in the second folder. The owner keeps a written *recovery
//! key* (this key, in groups): without it, a backup cannot be read on another computer.
//!
//! A database written before encryption (plain SQLite) is converted on the first open with a
//! key, and so are the backups beside it.

use crate::StoreError;
use sqlx::{ConnectOptions, Connection};
use std::path::{Path, PathBuf};

/// A 256-bit SQLCipher key. Never logged or shown, except as the recovery key to the owner.
#[derive(Clone, PartialEq, Eq)]
pub struct DbKey([u8; 32]);

impl std::fmt::Debug for DbKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DbKey([REDACTED])")
    }
}

/// Why a recovery key was not accepted.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("a recovery key is 64 letters and digits (0–9, A–F), in groups of 8")]
pub struct BadRecoveryKey;

impl DbKey {
    /// A new random key from the operating system.
    pub fn generate() -> Result<Self, StoreError> {
        let mut k = [0u8; 32];
        getrandom::fill(&mut k).map_err(|e| StoreError::io("generate database key", e))?;
        Ok(Self(k))
    }

    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    fn hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02X}")).collect()
    }

    /// How the owner writes it down: 8 groups of 8 hexadecimal characters.
    pub fn recovery_key(&self) -> String {
        self.hex()
            .as_bytes()
            .chunks(8)
            .map(|c| String::from_utf8_lossy(c).into_owned())
            .collect::<Vec<_>>()
            .join("-")
    }

    /// Reads a recovery key as written down: spaces, dashes and case do not matter.
    pub fn from_recovery_key(text: &str) -> Result<Self, BadRecoveryKey> {
        let hex: String = text.chars().filter(|c| !matches!(c, ' ' | '-' | '\t')).collect();
        if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(BadRecoveryKey);
        }
        let mut k = [0u8; 32];
        for (i, b) in k.iter_mut().enumerate() {
            *b = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).map_err(|_| BadRecoveryKey)?;
        }
        Ok(Self(k))
    }

    /// The `PRAGMA key` value: a raw key, `"x'…'"`.
    pub(crate) fn pragma(&self) -> String {
        format!("\"x'{}'\"", self.hex())
    }
}

/// What a database file is, from its first bytes.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FileKind {
    Missing,
    /// Plain SQLite (written before encryption, or in tests).
    Plain,
    /// Anything else: SQLCipher (or not a database at all).
    Encrypted,
}

pub fn file_kind(path: &Path) -> FileKind {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else {
        return FileKind::Missing;
    };
    let mut head = [0u8; 16];
    match f.read_exact(&mut head) {
        // An empty file is a database SQLite has not written yet: key it like a new one.
        Err(_) => FileKind::Missing,
        Ok(()) if &head == b"SQLite format 3\0" => FileKind::Plain,
        Ok(()) => FileKind::Encrypted,
    }
}

/// Connect options for a file, keyed if it is encrypted (or new) and a key is given.
pub fn keyed(
    options: sqlx::sqlite::SqliteConnectOptions,
    path: &Path,
    key: Option<&DbKey>,
) -> sqlx::sqlite::SqliteConnectOptions {
    match key {
        Some(k) if file_kind(path) != FileKind::Plain => options
            .pragma("key", k.pragma())
            // Wiping every freed allocation costs several times the query time and protects
            // only against reading this process's memory, which the key in memory does not
            // resist anyway (ADR-023).
            .pragma("cipher_memory_security", "OFF")
            // Every page read from disk is decrypted and authenticated; a page already in a
            // connection's cache is not. 32 MiB per connection (SQLite's default is 2 MiB)
            // keeps the working set decrypted.
            .pragma("cache_size", "-32768"),
        _ => options,
    }
}

/// Whether `key` opens the encrypted database at `path`.
pub async fn key_opens(path: &Path, key: &DbKey) -> bool {
    let opened = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(path)
        .read_only(true)
        .pragma("key", key.pragma())
        .connect()
        .await;
    match opened {
        Ok(mut c) => {
            let ok = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM sqlite_master")
                .fetch_one(&mut c)
                .await
                .is_ok();
            let _ = c.close().await;
            ok
        }
        Err(_) => false,
    }
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// Converts a plain SQLite file to SQLCipher with `key`, in place: exported to a temporary
/// file, synced, then renamed over the original (a crash leaves one whole file or the other).
/// The plain original is overwritten, not kept.
pub async fn encrypt_in_place(path: &Path, key: &DbKey) -> Result<(), StoreError> {
    if file_kind(path) != FileKind::Plain {
        return Ok(());
    }
    let target = sibling(path, ".encrypting");
    let _ = std::fs::remove_file(&target);
    // `create_if_missing`: ATTACH takes this connection's open flags, and must create the target.
    let mut c = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .connect()
        .await
        .map_err(StoreError::from_sqlx)?;
    // Fold any WAL into the file first, so the export sees every commit.
    sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .execute(&mut c)
        .await
        .map_err(StoreError::from_sqlx)?;
    let target_str = target
        .to_str()
        .ok_or_else(|| StoreError::io("encrypt database", std::io::Error::other("path is not UTF-8")))?;
    sqlx::query(&format!("ATTACH DATABASE ? AS encrypted KEY {}", key.pragma()))
        .bind(target_str)
        .execute(&mut c)
        .await
        .map_err(StoreError::from_sqlx)?;
    let exported = sqlx::query("SELECT sqlcipher_export('encrypted')")
        .execute(&mut c)
        .await;
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&mut c)
        .await
        .unwrap_or(0);
    if exported.is_ok() {
        sqlx::query(&format!("PRAGMA encrypted.user_version = {version}"))
            .execute(&mut c)
            .await
            .map_err(StoreError::from_sqlx)?;
    }
    sqlx::query("DETACH DATABASE encrypted")
        .execute(&mut c)
        .await
        .map_err(StoreError::from_sqlx)?;
    c.close().await.map_err(StoreError::from_sqlx)?;
    if let Err(e) = exported {
        let _ = std::fs::remove_file(&target);
        return Err(StoreError::from_sqlx(e));
    }
    crate::sync_file(&target).map_err(|e| StoreError::io("sync encrypted database", e))?;
    if !key_opens(&target, key).await {
        let _ = std::fs::remove_file(&target);
        return Err(StoreError::Corrupt(
            "the encrypted copy did not open with its key".into(),
        ));
    }
    for suffix in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(sibling(path, suffix));
    }
    std::fs::rename(&target, path).map_err(|e| StoreError::io("put encrypted database in place", e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_keys_round_trip_and_reject_typos() {
        let k = DbKey::from_bytes([0xAB; 32]);
        let r = k.recovery_key();
        assert_eq!(r.len(), 64 + 7);
        assert_eq!(r.split('-').count(), 8);
        assert_eq!(DbKey::from_recovery_key(&r).unwrap(), k);
        assert_eq!(
            DbKey::from_recovery_key(&r.to_lowercase().replace('-', " ")).unwrap(),
            k
        );
        assert!(DbKey::from_recovery_key(&r[..70]).is_err());
        assert!(DbKey::from_recovery_key(&r.replace('A', "G")).is_err());
        assert_eq!(format!("{k:?}"), "DbKey([REDACTED])");
        assert_ne!(DbKey::generate().unwrap(), DbKey::generate().unwrap());
    }
}
