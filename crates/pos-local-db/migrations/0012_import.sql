-- 0012: CSV imports (E06-T07, CAT-008). Spec: docs/backlog/04-local-database.md
-- Staging for an import: the job and every row of the file, as read, as parsed and as
-- checked. Local only: what syncs is the products the import creates.
-- Append-only: once merged to main this file is never edited.
CREATE TABLE import_jobs (
  job_id         TEXT PRIMARY KEY,
  kind           TEXT NOT NULL CHECK (kind IN ('products','opening_stock')),
  file_name      TEXT NOT NULL,
  file_sha256    TEXT NOT NULL,
  mapping_json   TEXT CHECK (mapping_json IS NULL OR json_valid(mapping_json)),
  options_json   TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(options_json)),  -- on_duplicate: skip|update|error
  status         TEXT NOT NULL CHECK (status IN ('parsed','mapped','validated','committing','committed','failed','cancelled')),
  total_rows     INTEGER NOT NULL DEFAULT 0,
  valid_rows     INTEGER NOT NULL DEFAULT 0,
  error_rows     INTEGER NOT NULL DEFAULT 0,
  created_by     TEXT NOT NULL,
  created_at     INTEGER NOT NULL,
  committed_at   INTEGER
) STRICT;

CREATE TABLE import_rows (
  job_id        TEXT NOT NULL REFERENCES import_jobs(job_id) ON DELETE CASCADE,
  row_no        INTEGER NOT NULL,
  raw_json      TEXT NOT NULL CHECK (json_valid(raw_json)),
  parsed_json   TEXT CHECK (parsed_json IS NULL OR json_valid(parsed_json)),
  status        TEXT NOT NULL CHECK (status IN ('pending','valid','error','duplicate_in_file','duplicate_existing','skipped','committed')),
  errors_json   TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(errors_json)),
  PRIMARY KEY (job_id, row_no)
) STRICT;
