-- 0008_returns: returns, their lines and refunds (immutable facts), linked to the original
-- sale (RET-003..007, INV-009). E12-T01/T02. Spec: docs/backlog/04-local-database.md#0008_returnssql
-- Append-only: once merged to main this file is never edited.

CREATE TABLE returns (
  return_id          TEXT PRIMARY KEY,
  business_id        TEXT NOT NULL,
  location_id        TEXT NOT NULL,
  register_id        TEXT NOT NULL,
  original_sale_id   TEXT NOT NULL REFERENCES sales(sale_id),   -- RET-003
  return_seq         INTEGER NOT NULL,
  return_number      TEXT NOT NULL,                              -- '{register_code}-R-{seq:06}'
  reason_code        TEXT NOT NULL CHECK (reason_code IN ('changed_mind','defective','wrong_item','damaged_in_transit','other')),
  note               TEXT,
  subtotal_minor     INTEGER NOT NULL,
  tax_minor          INTEGER NOT NULL,
  total_minor        INTEGER NOT NULL CHECK (total_minor >= 0),
  actor_user_id      TEXT NOT NULL,                              -- RET-006
  approver_user_id   TEXT,
  occurred_at        INTEGER NOT NULL,
  business_date      TEXT NOT NULL,
  created_at         INTEGER NOT NULL,
  UNIQUE (register_id, return_seq),
  UNIQUE (business_id, return_number)
) STRICT;

CREATE TABLE return_lines (
  return_line_id         TEXT PRIMARY KEY,
  return_id              TEXT NOT NULL REFERENCES returns(return_id),
  original_sale_line_id  TEXT NOT NULL REFERENCES sale_lines(sale_line_id),
  quantity_milli         INTEGER NOT NULL CHECK (quantity_milli > 0),
  refund_minor           INTEGER NOT NULL CHECK (refund_minor >= 0),
  tax_minor              INTEGER NOT NULL,
  disposition            TEXT NOT NULL CHECK (disposition IN ('restock','damaged','no_stock_change'))  -- INV-009
) STRICT;
CREATE INDEX ix_return_lines_original ON return_lines(original_sale_line_id);

CREATE TABLE refunds (                                  -- RET-005: separate from the return
  refund_id            TEXT PRIMARY KEY,
  return_id            TEXT NOT NULL REFERENCES returns(return_id),
  business_id          TEXT NOT NULL,
  seq                  INTEGER NOT NULL,
  tender_type          TEXT NOT NULL CHECK (tender_type IN ('cash','card_external','bank_transfer_external','qr_external','other_external')),
  amount_minor         INTEGER NOT NULL CHECK (amount_minor > 0),
  status               TEXT NOT NULL CHECK (status IN ('pending','confirmed','declined','reconciliation_required')),
  confirmation_source  TEXT NOT NULL CHECK (confirmation_source IN ('operator','provider')),
  external_reference   TEXT,
  recorded_by          TEXT NOT NULL,
  occurred_at          INTEGER NOT NULL,
  UNIQUE (return_id, seq)
) STRICT;

CREATE TRIGGER returns_no_update      BEFORE UPDATE ON returns      BEGIN SELECT RAISE(ABORT, 'returns are immutable'); END;
CREATE TRIGGER returns_no_delete      BEFORE DELETE ON returns      BEGIN SELECT RAISE(ABORT, 'returns are immutable'); END;
CREATE TRIGGER return_lines_no_update BEFORE UPDATE ON return_lines BEGIN SELECT RAISE(ABORT, 'return lines are immutable'); END;
CREATE TRIGGER return_lines_no_delete BEFORE DELETE ON return_lines BEGIN SELECT RAISE(ABORT, 'return lines are immutable'); END;
