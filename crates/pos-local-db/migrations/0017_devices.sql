-- 0017_devices: per-till device configuration and wider print adapters (E20-T11, ADR-023).
-- Append-only: once merged to main this file is never edited.

-- Printers, scanners, payment terminals and displays attached to THIS till. Local only and
-- never synced: an IP address or COM port belongs to one machine, not to the business.
CREATE TABLE devices (
  device_id    TEXT PRIMARY KEY,
  register_id  TEXT NOT NULL REFERENCES registers(register_id),
  kind         TEXT NOT NULL CHECK (kind IN ('receipt_printer','scanner','payment_terminal','customer_display')),
  role         TEXT NOT NULL DEFAULT 'main' CHECK (length(role) BETWEEN 1 AND 32),
  name         TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 80),
  config_json  TEXT NOT NULL CHECK (json_valid(config_json)),
  enabled      INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0,1)),
  version      INTEGER NOT NULL DEFAULT 1,
  created_at   INTEGER NOT NULL,
  updated_at   INTEGER NOT NULL,
  UNIQUE (register_id, kind, role)
) STRICT;

-- print_jobs.adapter named only the first planned adapters. Every transport now has its own
-- name. SQLite cannot alter a CHECK, so both print tables are rebuilt, keeping every row.
CREATE TABLE print_jobs_new (
  job_id          TEXT PRIMARY KEY,
  receipt_id      TEXT NOT NULL REFERENCES receipts(receipt_id),
  adapter         TEXT NOT NULL CHECK (adapter IN ('virtual','pdf','os_spooler','escpos_usb','escpos_net','escpos_serial','escpos_file')),
  target          TEXT,
  copy_kind       TEXT NOT NULL CHECK (copy_kind IN ('original','reprint')),
  status          TEXT NOT NULL CHECK (status IN ('queued','submitted','completed','failed','unknown','cancelled')),
  attempt_count   INTEGER NOT NULL DEFAULT 0,
  last_error_code TEXT,
  requested_by    TEXT NOT NULL,
  created_at      INTEGER NOT NULL,
  updated_at      INTEGER NOT NULL
) STRICT;
INSERT INTO print_jobs_new SELECT job_id, receipt_id, adapter, target, copy_kind, status, attempt_count,
  last_error_code, requested_by, created_at, updated_at FROM print_jobs;

CREATE TABLE print_job_events_new (
  job_id       TEXT NOT NULL REFERENCES print_jobs_new(job_id),
  seq          INTEGER NOT NULL,
  status       TEXT NOT NULL,
  detail_json  TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(detail_json)),
  occurred_at  INTEGER NOT NULL,
  PRIMARY KEY (job_id, seq)
) STRICT;
INSERT INTO print_job_events_new SELECT job_id, seq, status, detail_json, occurred_at FROM print_job_events;

DROP TABLE print_job_events;
DROP TABLE print_jobs;
-- Renaming also rewrites print_job_events_new's foreign key to point at print_jobs.
ALTER TABLE print_jobs_new RENAME TO print_jobs;
ALTER TABLE print_job_events_new RENAME TO print_job_events;
CREATE INDEX ix_print_jobs_active ON print_jobs(status) WHERE status IN ('queued','submitted');
