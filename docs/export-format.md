# Full data export format (E19-T04)

The owner's **Settings → Backups → Export all data…** saves everything one till holds as a ZIP
of CSV files. It is meant for the owner's records, an accountant, or moving to another system.
For putting a till back, use a backup instead (see `docs/runbooks/restore-drill.md`).

`scripts/read-export.py` reads it with nothing but Python 3:

```sh
python3 scripts/read-export.py check  sale-os-export-2026-11-18.zip           # is it whole?
python3 scripts/read-export.py sqlite sale-os-export-2026-11-18.zip back.db   # load into SQLite
python3 scripts/read-export.py rows   sale-os-export-2026-11-18.zip sales     # JSON lines
```

## Layout

```text
manifest.json
tables/<table>.csv     one per table of the till's database (docs/backlog/04-local-database.md)
```

All the tables are read in one read transaction, so they agree with each other (a sale and its
lines, tenders and stock movements are either all there or none). Sales can go on while the
export runs.

## manifest.json

| Field | Meaning |
|---|---|
| `format` | always `"sale-os-export"` |
| `formatVersion` | `1` (this document). A reader refuses versions it does not know |
| `appVersion` | the app that wrote it |
| `schemaVersion` | the database migration the till was at; table and column meanings are those of that version |
| `createdAtMs` | when, in milliseconds since 1970-01-01 UTC |
| `businessId`, `registerId` | the business and till (UUIDs) |
| `tables[]` | `{ name, file, rows, columns: [{ name, type, nullable }] }`; `type` is the SQLite declared type (`TEXT`, `INTEGER`) |
| `excludedColumns` | `table.column` names never written: `staff_users.pin_hash` |
| `excludedTables` | internal tables left out: SQLite's own, the migration log, the product search index (rebuilt from `products`) |
| `redactedJsonKeys` | keys whose values are `"[redacted]"` in every JSON column: `pinHash`, `pin_hash` |

## CSV files

- UTF-8 with a byte-order mark; CRLF after every record; the first record is the column names,
  in the manifest's order.
- RFC 4180 quoting: a field with a comma, quote, CR or LF is quoted, quotes doubled.
- **NULL is an empty, unquoted field.** Empty text is `""` (quoted). (Python's `csv` module
  reads both as empty; the script tells them apart.)
- Integers are written as they are stored. Money is in minor units (`*_minor`, for example
  pence), quantities in thousandths (`*_milli`), tax rates in parts per million (`*_ppm`), times
  in milliseconds since 1970-01-01 UTC (`*_at`), business dates as `YYYY-MM-DD`. JSON columns
  (`*_json`) hold JSON text.
- **Formula guard:** text starting with `=`, `+`, `-`, `@`, a tab, a CR or `'` gets a leading
  `'`, so a spreadsheet shows it rather than running it. A reader removes one leading `'` from
  any field that has one (only text can start with `'`).
- Binary values (none at schema 16) would be written as uppercase hexadecimal.

## What is not in it

- PIN hashes: the `pin_hash` column is left out, and `pinHash` inside event payloads (staff
  snapshots that sync between tills) reads `"[redacted]"`.
- Cloud credentials: the device's refresh token is in the operating system's keychain, never in
  the database. The owner's cloud password is never stored on the till.
- Card numbers: never stored anywhere (PRD §13).

## Audit

Each export is recorded as `data.exported` with the file's name (not its folder), the number of
tables and the number of rows. Cancelling the save dialog writes and records nothing.
