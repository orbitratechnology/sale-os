#!/usr/bin/env python3
"""Reads a Sale OS full data export (E19-T04). Format: docs/export-format.md.

  read-export.py check  EXPORT.zip            checks the export against its manifest
  read-export.py sqlite EXPORT.zip OUT.db     loads every table into a new SQLite database
  read-export.py rows   EXPORT.zip TABLE      prints a table's rows as JSON lines

Only the Python standard library is used. Exits non-zero if the export is not whole.
"""

import json
import sqlite3
import sys
import zipfile


def read_field(text, i):
    """One field from position i: (value, position after it)."""
    n = len(text)
    if i < n and text[i] == '"':
        i += 1
        value = []
        while True:
            j = text.index('"', i)
            value.append(text[i:j])
            if j + 1 < n and text[j + 1] == '"':
                value.append('"')
                i = j + 2
            else:
                i = j + 1
                break
        field = "".join(value)
    else:
        j = i
        while j < n and text[j] not in ",\r\n":
            j += 1
        field = text[i:j] if j > i else None
        i = j
    if field is not None and field.startswith("'"):
        field = field[1:]  # the formula guard (only text starts with ')
    return field, i


def parse_csv(text):
    """RFC 4180 records. An unquoted empty field is None (NULL); a quoted one is ''."""
    if text.startswith("\ufeff"):
        text = text[1:]
    records, i, n = [], 0, len(text)
    while i < n:
        record = []
        while True:
            field, i = read_field(text, i)
            record.append(field)
            if i < n and text[i] == ",":
                i += 1
                continue
            break
        if text.startswith("\r\n", i):
            i += 2
        elif i < n and text[i] in "\r\n":
            i += 1
        records.append(record)
    return records


def load(path):
    z = zipfile.ZipFile(path)
    bad = z.testzip()
    if bad is not None:
        raise SystemExit(f"{path}: {bad} is damaged (CRC mismatch)")
    manifest = json.loads(z.read("manifest.json"))
    if manifest.get("format") != "sale-os-export":
        raise SystemExit(f"{path}: not a Sale OS export")
    if manifest.get("formatVersion") != 1:
        raise SystemExit(f"{path}: format version {manifest.get('formatVersion')} is not supported")
    return z, manifest


def table_rows(z, table):
    records = parse_csv(z.read(table["file"]).decode("utf-8"))
    header, rows = records[0], records[1:]
    expected = [c["name"] for c in table["columns"]]
    if header != expected:
        raise SystemExit(f"{table['file']}: header {header} is not {expected}")
    return rows


def check(path):
    z, manifest = load(path)
    excluded = set(manifest["excludedColumns"])
    total = 0
    for t in manifest["tables"]:
        rows = table_rows(z, t)
        if len(rows) != t["rows"]:
            raise SystemExit(f"{t['file']}: {len(rows)} rows, the manifest says {t['rows']}")
        for c in t["columns"]:
            if f"{t['name']}.{c['name']}" in excluded:
                raise SystemExit(f"{t['file']}: {c['name']} must not be exported")
        for r in rows:
            if len(r) != len(t["columns"]):
                raise SystemExit(f"{t['file']}: a row has {len(r)} fields, not {len(t['columns'])}")
        total += len(rows)
    print(
        f"ok: {len(manifest['tables'])} tables, {total} rows, schema {manifest['schemaVersion']}, "
        f"app {manifest['appVersion']}"
    )


def to_sqlite(path, out):
    z, manifest = load(path)
    db = sqlite3.connect(out)
    for t in manifest["tables"]:
        cols = ", ".join(f'"{c["name"]}" {c["type"]}' for c in t["columns"])
        db.execute(f'CREATE TABLE "{t["name"]}" ({cols})')
        marks = ", ".join("?" for _ in t["columns"])
        db.executemany(f'INSERT INTO "{t["name"]}" VALUES ({marks})', table_rows(z, t))
    db.commit()
    print(f"wrote {out}: {len(manifest['tables'])} tables")


def rows(path, name):
    z, manifest = load(path)
    t = next((t for t in manifest["tables"] if t["name"] == name), None)
    if t is None:
        raise SystemExit(f"no table {name}")
    names = [c["name"] for c in t["columns"]]
    for r in table_rows(z, t):
        print(json.dumps(dict(zip(names, r)), ensure_ascii=False))


if __name__ == "__main__":
    args = sys.argv[1:]
    if len(args) == 2 and args[0] == "check":
        check(args[1])
    elif len(args) == 3 and args[0] == "sqlite":
        to_sqlite(args[1], args[2])
    elif len(args) == 3 and args[0] == "rows":
        rows(args[1], args[2])
    else:
        raise SystemExit(__doc__)
