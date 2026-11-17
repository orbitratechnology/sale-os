-- 0007_receipts_printing: immutable receipt documents and the local print queue.
-- E13-T02/T06, RCP-001..007, ADR-014. Spec: docs/backlog/04-local-database.md#0007_receipts_printingsql
-- Landed before returns, so it is 0007 (returns become 0008).
-- Append-only: once merged to main this file is never edited.

CREATE TABLE receipts (
  receipt_id       TEXT PRIMARY KEY,
  business_id      TEXT NOT NULL,
  source_type      TEXT NOT NULL CHECK (source_type IN ('sale','return','cash_session')),
  source_id        TEXT NOT NULL,
  content_version  INTEGER NOT NULL,          -- ReceiptDocument schema version
  document_json    TEXT NOT NULL CHECK (json_valid(document_json)),
  created_at       INTEGER NOT NULL,
  UNIQUE (source_type, source_id, content_version)
) STRICT;
CREATE TRIGGER receipts_no_update BEFORE UPDATE ON receipts BEGIN SELECT RAISE(ABORT, 'receipts are immutable'); END;

-- Local only (ADR-009).
CREATE TABLE print_jobs (
  job_id          TEXT PRIMARY KEY,
  receipt_id      TEXT NOT NULL REFERENCES receipts(receipt_id),
  adapter         TEXT NOT NULL CHECK (adapter IN ('virtual','pdf','os_spooler','escpos_usb','escpos_net')),
  target          TEXT,
  copy_kind       TEXT NOT NULL CHECK (copy_kind IN ('original','reprint')),       -- RCP-007
  status          TEXT NOT NULL CHECK (status IN ('queued','submitted','completed','failed','unknown','cancelled')),
  attempt_count   INTEGER NOT NULL DEFAULT 0,
  last_error_code TEXT,
  requested_by    TEXT NOT NULL,
  created_at      INTEGER NOT NULL,
  updated_at      INTEGER NOT NULL
) STRICT;
CREATE INDEX ix_print_jobs_active ON print_jobs(status) WHERE status IN ('queued','submitted');

CREATE TABLE print_job_events (
  job_id       TEXT NOT NULL REFERENCES print_jobs(job_id),
  seq          INTEGER NOT NULL,
  status       TEXT NOT NULL,
  detail_json  TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(detail_json)),
  occurred_at  INTEGER NOT NULL,
  PRIMARY KEY (job_id, seq)
) STRICT;
