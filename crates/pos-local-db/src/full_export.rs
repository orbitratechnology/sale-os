//! The full business data export (E19-T04, PRD §2.2 (7)): every table of this register's
//! database as CSV, in one ZIP with a `manifest.json` (format, schema version, tables, columns,
//! row counts). The format is described in `docs/export-format.md`, and
//! `scripts/read-export.py` reads it back.
//!
//! Secrets stay out: the columns in [`SECRET_COLUMNS`] are not written at all, and neither
//! are the full-text index's internal tables (they are rebuilt from `products`). Refresh
//! tokens are not in the database (they are in the OS keychain).
//!
//! Everything is read inside one read transaction, so the tables agree with each other; sales
//! go on meanwhile (WAL). The file is written under a temporary name and renamed when whole.

use crate::{LocalDb, MIGRATOR, StoreError};
use serde::Serialize;
use sqlx::Row;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

/// Columns never exported (`table`, `column`).
pub const SECRET_COLUMNS: &[(&str, &str)] = &[("staff_users", "pin_hash")];

/// Keys whose values are replaced with `"[redacted]"` wherever they appear in a JSON column
/// (event payloads carry `staff.upserted` snapshots, which hold the PIN hash so staff can sign
/// in on every register).
pub const SECRET_JSON_KEYS: &[&str] = &["pinHash", "pin_hash"];

/// The export format's version (`manifest.json` `formatVersion`).
pub const FORMAT_VERSION: u32 = 1;

/// Rows read per query.
const PAGE: i64 = 2_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnInfo {
    pub name: String,
    /// The declared SQLite type (`TEXT`, `INTEGER`, ...).
    #[serde(rename = "type")]
    pub declared_type: String,
    pub nullable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TableInfo {
    pub name: String,
    pub file: String,
    pub rows: u64,
    pub columns: Vec<ColumnInfo>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub format: &'static str,
    pub format_version: u32,
    pub app_version: String,
    pub schema_version: i64,
    /// Milliseconds since the Unix epoch, UTC.
    pub created_at_ms: i64,
    pub business_id: Option<String>,
    pub register_id: Option<String>,
    pub tables: Vec<TableInfo>,
    /// `table.column` names left out.
    pub excluded_columns: Vec<String>,
    /// Tables left out (internal, rebuilt from others).
    pub excluded_tables: Vec<String>,
    /// JSON keys whose values are `"[redacted]"` in every JSON column.
    pub redacted_json_keys: Vec<String>,
}

impl Manifest {
    pub fn total_rows(&self) -> u64 {
        self.tables.iter().map(|t| t.rows).sum()
    }
}

/// Writes the export to `path` and returns its manifest.
pub async fn write(
    db: &LocalDb,
    path: &Path,
    app_version: &str,
    created_at_ms: i64,
) -> Result<Manifest, StoreError> {
    let partial = path.with_extension("zip.partial");
    let result = write_to(db, &partial, app_version, created_at_ms).await;
    match result {
        Ok(manifest) => {
            crate::replace_file(&partial, path).map_err(|e| StoreError::io("export", e))?;
            Ok(manifest)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&partial);
            Err(e)
        }
    }
}

async fn write_to(
    db: &LocalDb,
    path: &Path,
    app_version: &str,
    created_at_ms: i64,
) -> Result<Manifest, StoreError> {
    let file = std::fs::File::create(path).map_err(|e| StoreError::io("export", e))?;
    let mut zip = Zip::new(BufWriter::new(file));
    let mut conn = db.readers().acquire().await.map_err(StoreError::from_sqlx)?;
    // One snapshot for every table.
    sqlx::query("BEGIN")
        .execute(&mut *conn)
        .await
        .map_err(StoreError::from_sqlx)?;
    let result = async {
        let (tables, excluded_tables) = tables(&mut conn).await?;
        let mut infos = Vec::with_capacity(tables.len());
        for t in tables {
            infos.push(export_table(&mut conn, &mut zip, &t).await?);
        }
        let ids: Option<(String, String)> =
            sqlx::query_as("SELECT business_id, register_id FROM registers LIMIT 1")
                .fetch_optional(&mut *conn)
                .await
                .map_err(StoreError::from_sqlx)?;
        let schema_version: i64 =
            sqlx::query_scalar("SELECT coalesce(max(version), 0) FROM _sqlx_migrations")
                .fetch_one(&mut *conn)
                .await
                .map_err(StoreError::from_sqlx)?;
        Ok::<_, StoreError>(Manifest {
            format: "sale-os-export",
            format_version: FORMAT_VERSION,
            app_version: app_version.to_owned(),
            schema_version,
            created_at_ms,
            business_id: ids.as_ref().map(|i| i.0.clone()),
            register_id: ids.map(|i| i.1),
            tables: infos,
            excluded_columns: SECRET_COLUMNS.iter().map(|(t, c)| format!("{t}.{c}")).collect(),
            excluded_tables,
            redacted_json_keys: SECRET_JSON_KEYS.iter().map(|k| (*k).to_owned()).collect(),
        })
    }
    .await;
    let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
    let manifest = result?;
    debug_assert!(manifest.schema_version <= MIGRATOR.iter().map(|m| m.version).max().unwrap_or(0));
    let json = serde_json::to_vec_pretty(&manifest).map_err(|e| StoreError::io("export", e))?;
    zip.start("manifest.json")?;
    zip.write_all(&json).map_err(|e| StoreError::io("export", e))?;
    zip.finish_entry()?;
    let mut out = zip.close()?;
    out.flush().map_err(|e| StoreError::io("export", e))?;
    out.get_ref()
        .sync_all()
        .map_err(|e| StoreError::io("export", e))?;
    Ok(manifest)
}

/// The tables to export, and the internal ones left out.
async fn tables(conn: &mut sqlx::SqliteConnection) -> Result<(Vec<String>, Vec<String>), StoreError> {
    let all: Vec<(String, String)> = sqlx::query_as(
        "SELECT name, coalesce(sql, '') FROM sqlite_schema WHERE type = 'table' ORDER BY name",
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(StoreError::from_sqlx)?;
    let virtual_tables: Vec<&str> = all
        .iter()
        .filter(|(_, sql)| sql.to_ascii_uppercase().starts_with("CREATE VIRTUAL TABLE"))
        .map(|(n, _)| n.as_str())
        .collect();
    let mut keep = Vec::new();
    let mut skip = Vec::new();
    for (name, _) in &all {
        let internal = name.starts_with("sqlite_")
            || name.starts_with("_sqlx")
            || virtual_tables
                .iter()
                .any(|v| name == v || name.starts_with(&format!("{v}_")));
        if internal {
            skip.push(name.clone());
        } else {
            keep.push(name.clone());
        }
    }
    Ok((keep, skip))
}

fn ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

async fn export_table<W: Write + Seek>(
    conn: &mut sqlx::SqliteConnection,
    zip: &mut Zip<W>,
    table: &str,
) -> Result<TableInfo, StoreError> {
    let cols: Vec<(String, String, i64)> =
        sqlx::query_as("SELECT name, type, \"notnull\" FROM pragma_table_info(?) ORDER BY cid")
            .bind(table)
            .fetch_all(&mut *conn)
            .await
            .map_err(StoreError::from_sqlx)?;
    let columns: Vec<ColumnInfo> = cols
        .into_iter()
        .filter(|(c, _, _)| !SECRET_COLUMNS.contains(&(table, c.as_str())))
        .map(|(name, declared_type, notnull)| ColumnInfo {
            name,
            declared_type,
            nullable: notnull == 0,
        })
        .collect();
    let file = format!("tables/{table}.csv");
    zip.start(&file)?;
    let io = |e: std::io::Error| StoreError::io("export", e);
    zip.write_all("\u{feff}".as_bytes()).map_err(io)?;
    let header: Vec<String> = columns.iter().map(|c| csv_field(Some(&c.name), "text")).collect();
    zip.write_all(header.join(",").as_bytes()).map_err(io)?;
    zip.write_all(b"\r\n").map_err(io)?;

    // Each value as text (blobs as hex), with its storage type so NULL and text are told apart.
    let select: Vec<String> = columns
        .iter()
        .map(|c| {
            let i = ident(&c.name);
            format!("typeof({i}), CASE typeof({i}) WHEN 'blob' THEN hex({i}) ELSE CAST({i} AS TEXT) END")
        })
        .collect();
    let sql = format!(
        "SELECT rowid, {} FROM {} WHERE rowid > ? ORDER BY rowid LIMIT {PAGE}",
        if select.is_empty() {
            "NULL".to_owned()
        } else {
            select.join(", ")
        },
        ident(table)
    );
    let mut after = i64::MIN;
    let mut rows = 0u64;
    loop {
        let page = sqlx::query(&sql)
            .bind(after)
            .fetch_all(&mut *conn)
            .await
            .map_err(StoreError::from_sqlx)?;
        let n = page.len();
        for row in &page {
            after = row.try_get(0).map_err(StoreError::from_sqlx)?;
            let mut line = Vec::with_capacity(columns.len());
            for i in 0..columns.len() {
                let kind: String = row.try_get(1 + 2 * i).map_err(StoreError::from_sqlx)?;
                let value: Option<String> = row.try_get(2 + 2 * i).map_err(StoreError::from_sqlx)?;
                let value = value.map(redact);
                line.push(csv_field(value.as_deref(), &kind));
            }
            zip.write_all(line.join(",").as_bytes()).map_err(io)?;
            zip.write_all(b"\r\n").map_err(io)?;
            rows += 1;
        }
        if n < usize::try_from(PAGE).unwrap_or(usize::MAX) {
            break;
        }
    }
    zip.finish_entry()?;
    Ok(TableInfo {
        name: table.to_owned(),
        file,
        rows,
        columns,
    })
}

/// A JSON value with the secret keys' values replaced; any other text unchanged.
pub fn redact(text: String) -> String {
    if !SECRET_JSON_KEYS.iter().any(|k| text.contains(k)) {
        return text;
    }
    fn walk(v: &mut serde_json::Value) {
        match v {
            serde_json::Value::Object(map) => {
                for (k, x) in map.iter_mut() {
                    if SECRET_JSON_KEYS.contains(&k.as_str()) {
                        *x = serde_json::Value::String("[redacted]".into());
                    } else {
                        walk(x);
                    }
                }
            }
            serde_json::Value::Array(items) => items.iter_mut().for_each(walk),
            _ => {}
        }
    }
    match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(mut v) if v.is_object() || v.is_array() => {
            walk(&mut v);
            v.to_string()
        }
        // Not JSON: a free-text field that happens to name a key. Nothing to redact.
        _ => text,
    }
}

/// One CSV field (RFC 4180). NULL is an empty field; empty text is `""`. Text that a
/// spreadsheet would run as a formula (or that starts with `'`) gets a leading `'`, which the
/// reader removes: only text can start with `'`.
pub fn csv_field(value: Option<&str>, storage_type: &str) -> String {
    let Some(v) = value else {
        return String::new();
    };
    let v = if storage_type == "text" && v.starts_with(['=', '+', '-', '@', '\t', '\r', '\'']) {
        format!("'{v}")
    } else {
        v.to_owned()
    };
    if v.is_empty() || v.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", v.replace('"', "\"\""))
    } else {
        v
    }
}

/// A minimal ZIP writer: deflated entries, each local header patched with its CRC and sizes
/// once the entry is written (so no data descriptors; every reader understands it). No ZIP64:
/// an entry or the archive over 4 GiB is refused.
struct Zip<W: Write + Seek> {
    out: W,
    entries: Vec<Entry>,
    current: Option<Current>,
}

struct Entry {
    name: String,
    crc: u32,
    compressed: u32,
    size: u32,
    offset: u32,
}

struct Current {
    name: String,
    offset: u64,
    data_start: u64,
    crc: crc32fast::Hasher,
    size: u64,
    deflate: flate2::write::DeflateEncoder<Vec<u8>>,
}

const DOS_TIME: u16 = 0;
/// 1 January 2000 (DOS dates start in 1980; the real time is in the manifest).
const DOS_DATE: u16 = (20 << 9) | (1 << 5) | 1;

fn too_big() -> StoreError {
    StoreError::io("export", std::io::Error::other("the export is over 4 GiB"))
}

impl<W: Write + Seek> Zip<W> {
    fn new(out: W) -> Self {
        Self {
            out,
            entries: Vec::new(),
            current: None,
        }
    }

    fn io(e: std::io::Error) -> StoreError {
        StoreError::io("export", e)
    }

    fn start(&mut self, name: &str) -> Result<(), StoreError> {
        let offset = self.out.stream_position().map_err(Self::io)?;
        let mut h = Vec::with_capacity(30 + name.len());
        h.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        h.extend_from_slice(&20u16.to_le_bytes()); // version needed
        h.extend_from_slice(&0x0800u16.to_le_bytes()); // UTF-8 names
        h.extend_from_slice(&8u16.to_le_bytes()); // deflate
        h.extend_from_slice(&DOS_TIME.to_le_bytes());
        h.extend_from_slice(&DOS_DATE.to_le_bytes());
        h.extend_from_slice(&[0; 12]); // crc, sizes: patched in finish_entry
        h.extend_from_slice(&u16::try_from(name.len()).map_err(|_| too_big())?.to_le_bytes());
        h.extend_from_slice(&0u16.to_le_bytes());
        h.extend_from_slice(name.as_bytes());
        self.out.write_all(&h).map_err(Self::io)?;
        self.current = Some(Current {
            name: name.to_owned(),
            offset,
            data_start: offset + h.len() as u64,
            crc: crc32fast::Hasher::new(),
            size: 0,
            deflate: flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default()),
        });
        Ok(())
    }

    fn finish_entry(&mut self) -> Result<(), StoreError> {
        let c = self
            .current
            .take()
            .ok_or_else(|| Self::io(std::io::Error::other("no entry started")))?;
        let rest = c.deflate.finish().map_err(Self::io)?;
        self.out.write_all(&rest).map_err(Self::io)?;
        let end = self.out.stream_position().map_err(Self::io)?;
        let crc = c.crc.finalize();
        let compressed = u32::try_from(end - c.data_start).map_err(|_| too_big())?;
        let size = u32::try_from(c.size).map_err(|_| too_big())?;
        self.out.seek(SeekFrom::Start(c.offset + 14)).map_err(Self::io)?;
        let mut patch = Vec::with_capacity(12);
        patch.extend_from_slice(&crc.to_le_bytes());
        patch.extend_from_slice(&compressed.to_le_bytes());
        patch.extend_from_slice(&size.to_le_bytes());
        self.out.write_all(&patch).map_err(Self::io)?;
        self.out.seek(SeekFrom::Start(end)).map_err(Self::io)?;
        self.entries.push(Entry {
            name: c.name,
            crc,
            compressed,
            size,
            offset: u32::try_from(c.offset).map_err(|_| too_big())?,
        });
        Ok(())
    }

    fn close(mut self) -> Result<W, StoreError> {
        let start = self.out.stream_position().map_err(Self::io)?;
        for e in &self.entries {
            let mut h = Vec::with_capacity(46 + e.name.len());
            h.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            h.extend_from_slice(&20u16.to_le_bytes()); // made by
            h.extend_from_slice(&20u16.to_le_bytes()); // needed
            h.extend_from_slice(&0x0800u16.to_le_bytes());
            h.extend_from_slice(&8u16.to_le_bytes());
            h.extend_from_slice(&DOS_TIME.to_le_bytes());
            h.extend_from_slice(&DOS_DATE.to_le_bytes());
            h.extend_from_slice(&e.crc.to_le_bytes());
            h.extend_from_slice(&e.compressed.to_le_bytes());
            h.extend_from_slice(&e.size.to_le_bytes());
            h.extend_from_slice(&u16::try_from(e.name.len()).map_err(|_| too_big())?.to_le_bytes());
            h.extend_from_slice(&[0; 8]); // extra, comment, disk, internal attributes
            h.extend_from_slice(&0u32.to_le_bytes()); // external attributes
            h.extend_from_slice(&e.offset.to_le_bytes());
            h.extend_from_slice(e.name.as_bytes());
            self.out.write_all(&h).map_err(Self::io)?;
        }
        let end = self.out.stream_position().map_err(Self::io)?;
        let count = u16::try_from(self.entries.len()).map_err(|_| too_big())?;
        let mut h = Vec::with_capacity(22);
        h.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        h.extend_from_slice(&[0; 4]); // disk numbers
        h.extend_from_slice(&count.to_le_bytes());
        h.extend_from_slice(&count.to_le_bytes());
        h.extend_from_slice(&u32::try_from(end - start).map_err(|_| too_big())?.to_le_bytes());
        h.extend_from_slice(&u32::try_from(start).map_err(|_| too_big())?.to_le_bytes());
        h.extend_from_slice(&0u16.to_le_bytes());
        self.out.write_all(&h).map_err(Self::io)?;
        Ok(self.out)
    }
}

impl<W: Write + Seek> Write for Zip<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let c = self
            .current
            .as_mut()
            .ok_or_else(|| std::io::Error::other("no entry started"))?;
        c.crc.update(buf);
        c.size += buf.len() as u64;
        c.deflate.write_all(buf)?;
        // Hand the compressed bytes on as they come, so memory stays small.
        let ready = std::mem::take(c.deflate.get_mut());
        self.out.write_all(&ready)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.out.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::csv_field;

    #[test]
    fn secrets_in_json_are_redacted() {
        use super::redact;
        assert_eq!(
            redact(r#"{"userId":"u","pinHash":"$argon2id$v=19$x","n":[{"pinHash":"y"}]}"#.into()),
            r#"{"n":[{"pinHash":"[redacted]"}],"pinHash":"[redacted]","userId":"u"}"#
        );
        assert_eq!(redact("plain pinHash text".into()), "plain pinHash text");
        assert_eq!(redact(r#"{"a":1}"#.into()), r#"{"a":1}"#);
    }

    #[test]
    fn fields() {
        assert_eq!(csv_field(None, "null"), "");
        assert_eq!(csv_field(Some(""), "text"), "\"\"");
        assert_eq!(csv_field(Some("-5"), "integer"), "-5");
        assert_eq!(csv_field(Some("-5"), "text"), "'-5");
        assert_eq!(csv_field(Some("=SUM(A1)"), "text"), "'=SUM(A1)");
        assert_eq!(csv_field(Some("'quoted"), "text"), "''quoted");
        assert_eq!(csv_field(Some("a,\"b\""), "text"), "\"a,\"\"b\"\"\"");
        assert_eq!(csv_field(Some("line\nbreak"), "text"), "\"line\nbreak\"");
    }
}
