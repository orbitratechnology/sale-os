-- D-02 (Sri Lanka): VAT tax invoices for registered buyers (Gazette 2481/22).
--
-- 1. `receipts` may hold a tax invoice document (source_type 'tax_invoice', source_id = the
--    sale). SQLite cannot change a CHECK, so the table is rebuilt with its rows, keeping its
--    name: print_jobs still references it. Foreign keys are checked at commit, after the rows
--    are back.
PRAGMA defer_foreign_keys = ON;

CREATE TABLE receipts_copy AS SELECT * FROM receipts;
DROP TABLE receipts;
CREATE TABLE receipts (
  receipt_id       TEXT PRIMARY KEY,
  business_id      TEXT NOT NULL,
  source_type      TEXT NOT NULL CHECK (source_type IN ('sale','return','cash_session','tax_invoice')),
  source_id        TEXT NOT NULL,
  content_version  INTEGER NOT NULL,          -- ReceiptDocument schema version
  document_json    TEXT NOT NULL CHECK (json_valid(document_json)),
  created_at       INTEGER NOT NULL,
  UNIQUE (source_type, source_id, content_version)
) STRICT;
INSERT INTO receipts (receipt_id, business_id, source_type, source_id, content_version, document_json, created_at)
  SELECT receipt_id, business_id, source_type, source_id, content_version, document_json, created_at FROM receipts_copy;
DROP TABLE receipts_copy;
CREATE TRIGGER receipts_no_update BEFORE UPDATE ON receipts BEGIN SELECT RAISE(ABORT, 'receipts are immutable'); END;

-- 2. One tax invoice per sale, numbered per register: YYMMM_<register code>_XXXXX. Local only
--    for now (not synced): the till keeps it, its backups and the full export carry it.
CREATE TABLE tax_invoices (
  tax_invoice_id     TEXT PRIMARY KEY,
  business_id        TEXT NOT NULL,
  register_id        TEXT NOT NULL REFERENCES registers(register_id),
  sale_id            TEXT NOT NULL UNIQUE REFERENCES sales(sale_id),
  invoice_seq        INTEGER NOT NULL CHECK (invoice_seq > 0),
  invoice_number     TEXT NOT NULL,
  purchaser_tin      TEXT NOT NULL,
  purchaser_name     TEXT NOT NULL,
  purchaser_address  TEXT NOT NULL CHECK (json_valid(purchaser_address)),
  issued_by          TEXT NOT NULL,
  issued_at          INTEGER NOT NULL,
  UNIQUE (register_id, invoice_seq),
  UNIQUE (business_id, invoice_number)
) STRICT;
CREATE TRIGGER tax_invoices_no_update BEFORE UPDATE ON tax_invoices BEGIN SELECT RAISE(ABORT, 'tax invoices are immutable'); END;
CREATE TRIGGER tax_invoices_no_delete BEFORE DELETE ON tax_invoices BEGIN SELECT RAISE(ABORT, 'tax invoices are immutable'); END;
