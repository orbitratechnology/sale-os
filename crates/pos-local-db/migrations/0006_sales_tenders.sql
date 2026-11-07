-- 0006_sales_tenders: completed sales, their lines and tenders (immutable facts).
-- E09-T08, E10-T01, POS-008..010. Spec: docs/backlog/04-local-database.md#0006_sales_tenderssql
-- Append-only: once merged to main this file is never edited.

CREATE TABLE sales (
  sale_id                      TEXT PRIMARY KEY,
  business_id                  TEXT NOT NULL,
  location_id                  TEXT NOT NULL,
  register_id                  TEXT NOT NULL,
  cart_id                      TEXT,
  cash_session_id              TEXT,
  receipt_seq                  INTEGER NOT NULL CHECK (receipt_seq > 0),
  receipt_number               TEXT NOT NULL,             -- '{prefix}{register_code}-{seq:06}', POS-009
  status                       TEXT NOT NULL DEFAULT 'completed' CHECK (status IN ('completed')),
  mode                         TEXT NOT NULL CHECK (mode IN ('sandbox','live')),  -- sandbox sales excluded from reports
  currency_code                TEXT NOT NULL,
  currency_exponent            INTEGER NOT NULL,
  prices_include_tax           INTEGER NOT NULL CHECK (prices_include_tax IN (0,1)),
  subtotal_minor               INTEGER NOT NULL,          -- Σ line gross
  line_discount_total_minor    INTEGER NOT NULL,
  order_discount_total_minor   INTEGER NOT NULL,
  tax_total_minor              INTEGER NOT NULL,
  rounding_minor               INTEGER NOT NULL DEFAULT 0, -- cash rounding (ADR-004)
  total_minor                  INTEGER NOT NULL,
  tendered_total_minor         INTEGER NOT NULL,
  change_minor                 INTEGER NOT NULL CHECK (change_minor >= 0),
  order_discount_json          TEXT CHECK (order_discount_json IS NULL OR json_valid(order_discount_json)),
  tax_breakdown_json           TEXT NOT NULL CHECK (json_valid(tax_breakdown_json)), -- [{tax_rate_id,name,rate_ppm,taxable_minor,tax_minor}]
  cashier_user_id              TEXT NOT NULL,
  approver_user_id             TEXT,
  occurred_at                  INTEGER NOT NULL,
  business_date                TEXT NOT NULL,
  timezone                     TEXT NOT NULL,
  created_at                   INTEGER NOT NULL,
  UNIQUE (register_id, receipt_seq),
  UNIQUE (business_id, receipt_number)
) STRICT;
CREATE INDEX ix_sales_date ON sales(business_date, mode);
CREATE INDEX ix_sales_receipt ON sales(receipt_number);

CREATE TABLE sale_lines (
  sale_line_id               TEXT PRIMARY KEY,
  sale_id                    TEXT NOT NULL REFERENCES sales(sale_id),
  business_id                TEXT NOT NULL,
  line_no                    INTEGER NOT NULL,
  product_id                 TEXT NOT NULL,
  variant_id                 TEXT NOT NULL,
  -- immutable snapshots (POS-010)
  name_snapshot              TEXT NOT NULL,
  variant_label_snapshot     TEXT NOT NULL,
  sku_snapshot               TEXT,
  barcode_snapshot           TEXT,
  unit_code                  TEXT NOT NULL,
  product_type               TEXT NOT NULL,
  quantity_milli             INTEGER NOT NULL CHECK (quantity_milli > 0),
  unit_price_minor           INTEGER NOT NULL,
  price_overridden           INTEGER NOT NULL DEFAULT 0 CHECK (price_overridden IN (0,1)),
  gross_minor                INTEGER NOT NULL,
  line_discount_minor        INTEGER NOT NULL,
  order_discount_alloc_minor INTEGER NOT NULL,
  tax_minor                  INTEGER NOT NULL,
  total_minor                INTEGER NOT NULL,          -- customer-facing line total
  discount_json              TEXT CHECK (discount_json IS NULL OR json_valid(discount_json)),
  tax_lines_json             TEXT NOT NULL CHECK (json_valid(tax_lines_json)),
  unit_cost_minor            INTEGER,                   -- cost snapshot for future margin (REP-007)
  UNIQUE (sale_id, line_no)
) STRICT;
CREATE INDEX ix_sale_lines_variant ON sale_lines(variant_id);

CREATE TABLE tenders (
  tender_id             TEXT PRIMARY KEY,
  sale_id               TEXT NOT NULL REFERENCES sales(sale_id),
  business_id           TEXT NOT NULL,
  seq                   INTEGER NOT NULL,
  tender_type           TEXT NOT NULL CHECK (tender_type IN ('cash','card_external','bank_transfer_external','qr_external','other_external')),
  amount_minor          INTEGER NOT NULL CHECK (amount_minor > 0),  -- amount applied to the sale
  received_minor        INTEGER,                                     -- cash handed over
  change_minor          INTEGER NOT NULL DEFAULT 0,
  status                TEXT NOT NULL CHECK (status IN ('pending','confirmed','declined','reversed','refunded','reconciliation_required')),
  confirmation_source   TEXT NOT NULL CHECK (confirmation_source IN ('operator','provider')),  -- PAY-003, AT-13
  provider_code         TEXT,
  external_reference    TEXT,
  recorded_by           TEXT NOT NULL,
  occurred_at           INTEGER NOT NULL,
  UNIQUE (sale_id, seq),
  CHECK (tender_type = 'cash' OR change_minor = 0)     -- change only from cash
) STRICT;

CREATE TABLE tender_status_events (                    -- append-only history; tenders.status is the projection
  tender_event_id   TEXT PRIMARY KEY,
  tender_id         TEXT NOT NULL REFERENCES tenders(tender_id),
  from_status       TEXT,
  to_status         TEXT NOT NULL,
  source            TEXT NOT NULL CHECK (source IN ('operator','provider','system')),
  detail_json       TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(detail_json)),
  actor_user_id     TEXT,
  occurred_at       INTEGER NOT NULL
) STRICT;

CREATE TRIGGER sales_no_update      BEFORE UPDATE ON sales      BEGIN SELECT RAISE(ABORT, 'sales are immutable'); END;
CREATE TRIGGER sales_no_delete      BEFORE DELETE ON sales      BEGIN SELECT RAISE(ABORT, 'sales are immutable'); END;
CREATE TRIGGER sale_lines_no_update BEFORE UPDATE ON sale_lines BEGIN SELECT RAISE(ABORT, 'sale lines are immutable'); END;
CREATE TRIGGER sale_lines_no_delete BEFORE DELETE ON sale_lines BEGIN SELECT RAISE(ABORT, 'sale lines are immutable'); END;
CREATE TRIGGER tenders_no_delete    BEFORE DELETE ON tenders    BEGIN SELECT RAISE(ABORT, 'tenders are immutable'); END;
CREATE TRIGGER tenders_only_status  BEFORE UPDATE OF tender_id, sale_id, tender_type, amount_minor, received_minor, change_minor ON tenders
BEGIN SELECT RAISE(ABORT, 'only tender status may change'); END;
CREATE TRIGGER tender_events_no_update BEFORE UPDATE ON tender_status_events BEGIN SELECT RAISE(ABORT, 'append-only'); END;
