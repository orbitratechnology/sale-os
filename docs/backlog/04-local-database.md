# 04 — Local Database (SQLite): Migrations, Invariants, Indexes

The DDL below is the **target schema**. Each migration is created by the task named in its
heading. Copy the DDL into `crates/pos-local-db/migrations/NNNN_name.sql` when that task is
implemented, and adjust it there through review. After it merges, the migration file is
authoritative and this document is updated to match.

Global rules (ADR-001, -002, -005, -006, -007):

- Every table is `STRICT`. IDs are `TEXT` UUIDv7.
- Money columns end in `_minor` (`INTEGER`). Quantities end in `_milli` (`INTEGER`). Rates end
  in `_ppm` (`INTEGER`). Instants end in `_at` (`INTEGER`, UTC ms).
- Business dates are `TEXT 'YYYY-MM-DD'`.
- Booleans are `INTEGER CHECK (x IN (0,1))`.
- JSON columns end in `_json` and are validated with `CHECK (json_valid(x))`.
- **Append-only tables** have `BEFORE UPDATE` and `BEFORE DELETE` triggers that `RAISE(ABORT)`.
  Projections are separate tables.
- Every business-owned table carries `business_id`. The local database holds one business, but
  the column keeps local and cloud payloads the same shape, and lets a future "switch
  workspace" feature stay simple.

Migration order and owner tasks:

| # | File | Created by | Stage |
|---|---|---|---|
| 0001 | `0001_core.sql` | E02-T02 (skeleton), E04-T01 (complete) | 0 → 1 |
| 0002 | `0002_staff.sql` | E05-T01 | 1 |
| 0003 | `0003_catalogue.sql` | E06-T01 | 1 |
| 0004 | `0004_carts.sql` | E09-T01 | 1 |
| 0005 | `0005_inventory.sql` | E11-T01 | 2 |
| 0006 | `0006_sales_tenders.sql` | E09-T08 / E10-T01 | 2 |
| 0007 | `0007_returns.sql` | E12-T01 | 2 |
| 0008 | `0008_receipts_printing.sql` | E13-T02 / E13-T06 | 2 |
| 0009 | `0009_cash_sessions.sql` | E15-T02 | 2 |
| 0010 | `0010_import.sql` | E06-T07 | 1 |
| 0011 | `0011_sync.sql` | E17-T01 (`outbox_events` itself ships in 0001) | 3 |

> Stage 0 ships a **throwaway** spike schema in a separate `spike/` directory. It is deleted
> before Stage 1. The real migrations start at 0001 and are never edited after they merge.

---

## 0001_core.sql

```sql
CREATE TABLE businesses (
  business_id        TEXT PRIMARY KEY,
  name               TEXT NOT NULL CHECK (length(trim(name)) > 0),
  legal_name         TEXT,
  tax_registration_no TEXT,
  address_json       TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(address_json)),
  phone              TEXT,
  email              TEXT,
  logo_path          TEXT,                         -- app-data relative path; object storage key after sync (P1)
  country_code       TEXT NOT NULL CHECK (length(country_code) = 2),
  timezone           TEXT NOT NULL,                -- IANA, validated in domain
  currency_code      TEXT NOT NULL CHECK (length(currency_code) = 3),
  currency_exponent  INTEGER NOT NULL CHECK (currency_exponent BETWEEN 0 AND 4),
  prices_include_tax INTEGER NOT NULL CHECK (prices_include_tax IN (0,1)),
  version            INTEGER NOT NULL DEFAULT 1,
  sync_status        TEXT NOT NULL DEFAULT 'pending' CHECK (sync_status IN ('pending','synced','conflict')),
  created_at         INTEGER NOT NULL,
  updated_at         INTEGER NOT NULL
) STRICT;

CREATE TABLE locations (
  location_id   TEXT PRIMARY KEY,
  business_id   TEXT NOT NULL REFERENCES businesses(business_id),
  name          TEXT NOT NULL,
  address_json  TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(address_json)),
  status        TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active','archived')),
  version       INTEGER NOT NULL DEFAULT 1,
  sync_status   TEXT NOT NULL DEFAULT 'pending' CHECK (sync_status IN ('pending','synced','conflict')),
  created_at    INTEGER NOT NULL,
  updated_at    INTEGER NOT NULL
) STRICT;

CREATE TABLE registers (
  register_id     TEXT PRIMARY KEY,
  business_id     TEXT NOT NULL REFERENCES businesses(business_id),
  location_id     TEXT NOT NULL REFERENCES locations(location_id),
  name            TEXT NOT NULL,
  code            TEXT NOT NULL CHECK (code GLOB '[A-Z0-9]*' AND length(code) BETWEEN 1 AND 6), -- used in receipt numbers
  receipt_prefix  TEXT NOT NULL DEFAULT '' CHECK (length(receipt_prefix) <= 8),
  status          TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active','retired')),
  version         INTEGER NOT NULL DEFAULT 1,
  sync_status     TEXT NOT NULL DEFAULT 'pending' CHECK (sync_status IN ('pending','synced','conflict')),
  created_at      INTEGER NOT NULL,
  updated_at      INTEGER NOT NULL,
  UNIQUE (business_id, code)
) STRICT;

-- Which register THIS installation is. Singleton row.
CREATE TABLE device_identity (
  id               INTEGER PRIMARY KEY CHECK (id = 1),
  installation_id  TEXT NOT NULL,                  -- random per install, never reused
  business_id      TEXT REFERENCES businesses(business_id),
  register_id      TEXT REFERENCES registers(register_id),
  mode             TEXT NOT NULL DEFAULT 'setup' CHECK (mode IN ('setup','sandbox','live')),  -- PRD §7.1 steps 8-9
  created_at       INTEGER NOT NULL
) STRICT;

CREATE TABLE business_settings (
  business_id  TEXT NOT NULL REFERENCES businesses(business_id),
  key          TEXT NOT NULL,                      -- e.g. 'receipt.footer', 'inventory.allow_negative'
  value_json   TEXT NOT NULL CHECK (json_valid(value_json)),
  version      INTEGER NOT NULL DEFAULT 1,
  sync_status  TEXT NOT NULL DEFAULT 'pending' CHECK (sync_status IN ('pending','synced','conflict')),
  updated_at   INTEGER NOT NULL,
  PRIMARY KEY (business_id, key)
) STRICT;

-- Register-local monotonic counters. Allocated only inside LocalStore::apply.
CREATE TABLE register_counters (
  register_id  TEXT NOT NULL REFERENCES registers(register_id),
  name         TEXT NOT NULL CHECK (name IN ('outbox_seq','receipt_seq','return_seq','inventory_doc_seq')),
  value        INTEGER NOT NULL DEFAULT 0 CHECK (value >= 0),
  PRIMARY KEY (register_id, name)
) STRICT;

CREATE TABLE audit_events (
  audit_id          TEXT PRIMARY KEY,
  business_id       TEXT NOT NULL,
  register_id       TEXT,
  actor_user_id     TEXT,                           -- NULL only for system actions (e.g. migration, sync)
  approver_user_id  TEXT,
  action            TEXT NOT NULL,                  -- catalogued, see 07-contracts §5
  entity_type       TEXT NOT NULL,
  entity_id         TEXT NOT NULL,
  occurred_at       INTEGER NOT NULL,
  metadata_json     TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(metadata_json)),
  outbox_event_id   TEXT
) STRICT;
CREATE INDEX ix_audit_entity ON audit_events(entity_type, entity_id);
CREATE INDEX ix_audit_time   ON audit_events(occurred_at);
CREATE TRIGGER audit_events_no_update BEFORE UPDATE ON audit_events BEGIN SELECT RAISE(ABORT, 'audit_events is append-only'); END;
CREATE TRIGGER audit_events_no_delete BEFORE DELETE ON audit_events BEGIN SELECT RAISE(ABORT, 'audit_events is append-only'); END;

-- Transactional outbox (ADR-008/009). Status columns ARE updated (delivery state), payload never is.
CREATE TABLE outbox_events (
  event_id          TEXT PRIMARY KEY,
  business_id       TEXT NOT NULL,
  location_id       TEXT NOT NULL,
  register_id       TEXT NOT NULL,
  local_sequence    INTEGER NOT NULL CHECK (local_sequence > 0),
  event_type        TEXT NOT NULL,                  -- e.g. 'sale.completed'
  schema_version    INTEGER NOT NULL,
  aggregate_type    TEXT NOT NULL,
  aggregate_id      TEXT NOT NULL,
  base_version      INTEGER,                        -- master-data events only (ADR-011)
  occurred_at       INTEGER NOT NULL,
  payload_json      TEXT NOT NULL CHECK (json_valid(payload_json)),
  status            TEXT NOT NULL DEFAULT 'pending'
                     CHECK (status IN ('pending','in_flight','applied','duplicate','conflict','rejected')),
  attempt_count     INTEGER NOT NULL DEFAULT 0,
  next_attempt_at   INTEGER,
  last_error_code   TEXT,
  server_result_json TEXT CHECK (server_result_json IS NULL OR json_valid(server_result_json)),
  handled_at        INTEGER,
  created_at        INTEGER NOT NULL,
  UNIQUE (register_id, local_sequence)
) STRICT;
CREATE INDEX ix_outbox_pending ON outbox_events(status, local_sequence);
CREATE TRIGGER outbox_payload_immutable BEFORE UPDATE OF event_id, local_sequence, event_type, schema_version, payload_json, base_version ON outbox_events
BEGIN SELECT RAISE(ABORT, 'outbox payload is immutable'); END;
CREATE TRIGGER outbox_no_delete BEFORE DELETE ON outbox_events
  WHEN OLD.status NOT IN ('applied','duplicate') BEGIN SELECT RAISE(ABORT, 'cannot delete unhandled outbox event'); END;

CREATE TABLE sync_state (
  id                       INTEGER PRIMARY KEY CHECK (id = 1),
  link_state               TEXT NOT NULL DEFAULT 'unlinked' CHECK (link_state IN ('unlinked','linking','linked','revoked')),
  cloud_base_url           TEXT,
  device_id                TEXT,
  last_pull_cursor         TEXT,
  last_ack_sequence        INTEGER NOT NULL DEFAULT 0,
  last_push_ok_at          INTEGER,
  last_pull_ok_at          INTEGER,
  last_error_code          TEXT,
  last_error_message       TEXT,
  server_clock_skew_ms     INTEGER
) STRICT;
```

Invariants tested in `E02-T03`:

- `register_counters.outbox_seq` equals `MAX(outbox_events.local_sequence)` for the register.
- `outbox_events` sequences have no gaps.

---

## 0002_staff.sql

```sql
CREATE TABLE staff_users (
  user_id         TEXT PRIMARY KEY,
  business_id     TEXT NOT NULL REFERENCES businesses(business_id),
  display_name    TEXT NOT NULL CHECK (length(trim(display_name)) > 0),
  role            TEXT NOT NULL CHECK (role IN ('owner','manager','cashier')),
  pin_hash        TEXT NOT NULL,                   -- PHC string, argon2id (ADR-013)
  pin_updated_at  INTEGER NOT NULL,
  must_change_pin INTEGER NOT NULL DEFAULT 0 CHECK (must_change_pin IN (0,1)),
  status          TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active','deactivated')),
  version         INTEGER NOT NULL DEFAULT 1,
  sync_status     TEXT NOT NULL DEFAULT 'pending' CHECK (sync_status IN ('pending','synced','conflict')),
  created_at      INTEGER NOT NULL,
  updated_at      INTEGER NOT NULL
) STRICT;
-- At least one active owner must always exist: enforced in domain (StaffService) + test, not SQL.

-- Device-local, never synced.
CREATE TABLE staff_login_throttle (
  user_id          TEXT PRIMARY KEY REFERENCES staff_users(user_id),
  failed_attempts  INTEGER NOT NULL DEFAULT 0,
  locked_until     INTEGER,
  last_failed_at   INTEGER
) STRICT;

CREATE TABLE staff_sessions (
  session_id   TEXT PRIMARY KEY,
  user_id      TEXT NOT NULL REFERENCES staff_users(user_id),
  register_id  TEXT NOT NULL REFERENCES registers(register_id),
  started_at   INTEGER NOT NULL,
  ended_at     INTEGER,
  end_reason   TEXT CHECK (end_reason IN ('logout','inactivity_lock','app_exit','switch_user'))
) STRICT;
```

---

## 0003_catalogue.sql

```sql
CREATE TABLE units (                                -- reference data, seeded, not synced
  unit_code        TEXT PRIMARY KEY,               -- 'ea','kg','g','l','ml','m','cm','box','pack'
  name             TEXT NOT NULL,
  allows_fraction  INTEGER NOT NULL CHECK (allows_fraction IN (0,1))
) STRICT;
INSERT INTO units VALUES ('ea','Each',0),('kg','Kilogram',1),('g','Gram',1),('l','Litre',1),
  ('ml','Millilitre',1),('m','Metre',1),('cm','Centimetre',1),('box','Box',0),('pack','Pack',0);

CREATE TABLE tax_rates (
  tax_rate_id     TEXT PRIMARY KEY,
  business_id     TEXT NOT NULL REFERENCES businesses(business_id),
  name            TEXT NOT NULL,                   -- printed on receipt, e.g. 'VAT 15%'
  rate_ppm        INTEGER NOT NULL CHECK (rate_ppm BETWEEN 0 AND 1000000),
  effective_from  TEXT,                            -- business date, inclusive; NULL = always
  effective_to    TEXT,                            -- business date, exclusive; NULL = open
  status          TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active','archived')),
  version         INTEGER NOT NULL DEFAULT 1,
  sync_status     TEXT NOT NULL DEFAULT 'pending' CHECK (sync_status IN ('pending','synced','conflict')),
  created_at      INTEGER NOT NULL,
  updated_at      INTEGER NOT NULL
) STRICT;

CREATE TABLE tax_categories (
  tax_category_id TEXT PRIMARY KEY,
  business_id     TEXT NOT NULL REFERENCES businesses(business_id),
  name            TEXT NOT NULL,                   -- 'Standard', 'Zero-rated', 'Exempt'
  is_default      INTEGER NOT NULL DEFAULT 0 CHECK (is_default IN (0,1)),
  status          TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active','archived')),
  version         INTEGER NOT NULL DEFAULT 1,
  sync_status     TEXT NOT NULL DEFAULT 'pending' CHECK (sync_status IN ('pending','synced','conflict')),
  created_at      INTEGER NOT NULL,
  updated_at      INTEGER NOT NULL
) STRICT;
CREATE UNIQUE INDEX ux_tax_category_default ON tax_categories(business_id) WHERE is_default = 1;

CREATE TABLE tax_category_rates (                   -- part of the tax_category aggregate
  tax_category_id TEXT NOT NULL REFERENCES tax_categories(tax_category_id),
  tax_rate_id     TEXT NOT NULL REFERENCES tax_rates(tax_rate_id),
  PRIMARY KEY (tax_category_id, tax_rate_id)
) STRICT;

CREATE TABLE categories (
  category_id  TEXT PRIMARY KEY,
  business_id  TEXT NOT NULL REFERENCES businesses(business_id),
  parent_id    TEXT REFERENCES categories(category_id),
  name         TEXT NOT NULL,
  sort_order   INTEGER NOT NULL DEFAULT 0,
  status       TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active','archived')),
  version      INTEGER NOT NULL DEFAULT 1,
  sync_status  TEXT NOT NULL DEFAULT 'pending' CHECK (sync_status IN ('pending','synced','conflict')),
  created_at   INTEGER NOT NULL,
  updated_at   INTEGER NOT NULL
) STRICT;

CREATE TABLE products (
  product_id       TEXT PRIMARY KEY,
  business_id      TEXT NOT NULL REFERENCES businesses(business_id),
  name             TEXT NOT NULL CHECK (length(trim(name)) > 0),
  description      TEXT,
  category_id      TEXT REFERENCES categories(category_id),
  product_type     TEXT NOT NULL CHECK (product_type IN ('stock','non_stock','service')),  -- CAT-006
  unit_code        TEXT NOT NULL REFERENCES units(unit_code),
  tax_category_id  TEXT NOT NULL REFERENCES tax_categories(tax_category_id),
  quick_select     INTEGER NOT NULL DEFAULT 0 CHECK (quick_select IN (0,1)),              -- POS-002 quick-select grid
  status           TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active','archived')),
  version          INTEGER NOT NULL DEFAULT 1,      -- aggregate version: product + variants + barcodes
  sync_status      TEXT NOT NULL DEFAULT 'pending' CHECK (sync_status IN ('pending','synced','conflict')),
  created_at       INTEGER NOT NULL,
  updated_at       INTEGER NOT NULL
) STRICT;
CREATE INDEX ix_products_category ON products(category_id) WHERE status = 'active';

CREATE TABLE product_variants (
  variant_id                 TEXT PRIMARY KEY,
  business_id                TEXT NOT NULL,
  product_id                 TEXT NOT NULL REFERENCES products(product_id),
  is_default                 INTEGER NOT NULL DEFAULT 0 CHECK (is_default IN (0,1)),   -- ADR-019
  sku                        TEXT,
  options_json               TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(options_json)), -- {"size":"M","colour":"Red"}
  label                      TEXT NOT NULL DEFAULT '',                                    -- derived display suffix "M / Red"
  price_minor                INTEGER NOT NULL CHECK (price_minor >= 0),
  cost_minor                 INTEGER CHECK (cost_minor IS NULL OR cost_minor >= 0),
  track_stock                INTEGER NOT NULL CHECK (track_stock IN (0,1)),
  low_stock_threshold_milli  INTEGER CHECK (low_stock_threshold_milli IS NULL OR low_stock_threshold_milli >= 0),
  scan_qty_milli             INTEGER NOT NULL DEFAULT 1000 CHECK (scan_qty_milli > 0),  -- BAR-005 scan unit
  sort_order                 INTEGER NOT NULL DEFAULT 0,
  status                     TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active','archived')),
  created_at                 INTEGER NOT NULL,
  updated_at                 INTEGER NOT NULL
) STRICT;
CREATE UNIQUE INDEX ux_variant_sku      ON product_variants(business_id, sku COLLATE NOCASE) WHERE sku IS NOT NULL AND status = 'active';
CREATE UNIQUE INDEX ux_variant_default  ON product_variants(product_id) WHERE is_default = 1;
CREATE INDEX        ix_variant_product  ON product_variants(product_id);

CREATE TABLE product_barcodes (
  barcode_id        TEXT PRIMARY KEY,
  business_id       TEXT NOT NULL,
  variant_id        TEXT NOT NULL REFERENCES product_variants(variant_id),
  value_normalized  TEXT NOT NULL,                 -- see E06-T03 normalization rules
  value_raw         TEXT NOT NULL,
  symbology         TEXT NOT NULL DEFAULT 'unknown'
                     CHECK (symbology IN ('ean13','ean8','upca','upce','code128','code39','itf14','qr','datamatrix','internal','unknown')),
  qualifier         TEXT NOT NULL DEFAULT 'alias' CHECK (qualifier IN ('primary','alias','pack')),
  pack_qty_milli    INTEGER NOT NULL DEFAULT 1000 CHECK (pack_qty_milli > 0),             -- 'pack' barcode adds N units
  status            TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active','archived')),
  created_at        INTEGER NOT NULL
) STRICT;
CREATE UNIQUE INDEX ux_barcode_value ON product_barcodes(business_id, value_normalized) WHERE status = 'active';  -- CAT-004, BAR-004
CREATE INDEX ix_barcode_variant ON product_barcodes(variant_id);

-- FTS5 for name/SKU/barcode search (ADR-015). Maintained by LocalStore within the same txn.
CREATE VIRTUAL TABLE product_search USING fts5(
  variant_id UNINDEXED,
  name, label, sku, barcodes, category,
  tokenize = 'unicode61 remove_diacritics 2',
  prefix = '2 3'
);
```

---

## 0004_carts.sql (draft carts, local only, never synced; ADR-009)

```sql
CREATE TABLE carts (
  cart_id              TEXT PRIMARY KEY,
  business_id          TEXT NOT NULL,
  register_id          TEXT NOT NULL REFERENCES registers(register_id),
  status               TEXT NOT NULL CHECK (status IN ('open','parked','voided','completed')),
  opened_by            TEXT NOT NULL REFERENCES staff_users(user_id),
  order_discount_json  TEXT CHECK (order_discount_json IS NULL OR json_valid(order_discount_json)),
  revision             INTEGER NOT NULL DEFAULT 0,  -- bumps on every mutation; UI sends it back on complete
  sale_id              TEXT,                        -- set when completed
  void_reason          TEXT,
  opened_at            INTEGER NOT NULL,
  updated_at           INTEGER NOT NULL,
  closed_at            INTEGER
) STRICT;
CREATE UNIQUE INDEX ux_one_open_cart ON carts(register_id) WHERE status = 'open';  -- POS-011 recovery target

CREATE TABLE cart_lines (
  cart_line_id               TEXT PRIMARY KEY,
  cart_id                    TEXT NOT NULL REFERENCES carts(cart_id) ON DELETE CASCADE,
  position                   INTEGER NOT NULL,
  variant_id                 TEXT NOT NULL REFERENCES product_variants(variant_id),
  quantity_milli             INTEGER NOT NULL CHECK (quantity_milli > 0),
  unit_price_override_minor  INTEGER CHECK (unit_price_override_minor IS NULL OR unit_price_override_minor >= 0),
  line_discount_json         TEXT CHECK (line_discount_json IS NULL OR json_valid(line_discount_json)),
  added_via                  TEXT NOT NULL CHECK (added_via IN ('scan','search','quick_select','manual_code')),
  last_scan_event_id         TEXT,                  -- BAR-007 duplicate-delivery guard
  created_at                 INTEGER NOT NULL,
  updated_at                 INTEGER NOT NULL
) STRICT;
CREATE INDEX ix_cart_lines_cart ON cart_lines(cart_id, position);

-- Processed scan events (dedupe window: last 500 per register, pruned by store).
CREATE TABLE scan_events_seen (
  scan_event_id  TEXT PRIMARY KEY,
  register_id    TEXT NOT NULL,
  received_at    INTEGER NOT NULL
) STRICT;
```

---

## 0005_inventory.sql

```sql
CREATE TABLE inventory_documents (
  document_id      TEXT PRIMARY KEY,
  business_id      TEXT NOT NULL,
  location_id      TEXT NOT NULL REFERENCES locations(location_id),
  register_id      TEXT NOT NULL,
  document_no      TEXT NOT NULL,                    -- e.g. 'R01-IN-000042'
  doc_type         TEXT NOT NULL CHECK (doc_type IN ('opening','receipt','adjustment','write_off')),
  reason_code      TEXT CHECK (reason_code IN ('damage','loss','theft','count_correction','expired','other')),
  supplier_name    TEXT,                              -- INV-005 minimal supplier
  reference        TEXT,
  note             TEXT,
  actor_user_id    TEXT NOT NULL,
  approver_user_id TEXT,
  occurred_at      INTEGER NOT NULL,
  business_date    TEXT NOT NULL,
  created_at       INTEGER NOT NULL,
  UNIQUE (business_id, document_no)
) STRICT;

CREATE TABLE stock_movements (
  movement_id           TEXT PRIMARY KEY,
  business_id           TEXT NOT NULL,
  location_id           TEXT NOT NULL REFERENCES locations(location_id),
  variant_id            TEXT NOT NULL REFERENCES product_variants(variant_id),
  bucket                TEXT NOT NULL DEFAULT 'sellable' CHECK (bucket IN ('sellable','damaged')),   -- ADR-020
  movement_type         TEXT NOT NULL CHECK (movement_type IN
                          ('opening','receipt','sale','return_restock','return_damaged',
                           'damage','loss','adjustment','write_off','correction')),
  quantity_delta_milli  INTEGER NOT NULL CHECK (quantity_delta_milli <> 0),
  unit_cost_minor       INTEGER,
  source_type           TEXT NOT NULL CHECK (source_type IN ('sale','return','inventory_document')),
  source_id             TEXT NOT NULL,
  source_line_id        TEXT,
  register_id           TEXT NOT NULL,
  actor_user_id         TEXT NOT NULL,
  occurred_at           INTEGER NOT NULL,
  business_date         TEXT NOT NULL,
  created_at            INTEGER NOT NULL
) STRICT;
CREATE INDEX ix_movements_variant ON stock_movements(variant_id, location_id, occurred_at);
CREATE INDEX ix_movements_source  ON stock_movements(source_type, source_id);
CREATE TRIGGER stock_movements_no_update BEFORE UPDATE ON stock_movements BEGIN SELECT RAISE(ABORT, 'stock_movements is append-only'); END;
CREATE TRIGGER stock_movements_no_delete BEFORE DELETE ON stock_movements BEGIN SELECT RAISE(ABORT, 'stock_movements is append-only'); END;

-- Projection (INV-004). Rebuildable from stock_movements (E11-T06). Includes movements pulled from other registers.
CREATE TABLE stock_balances (
  business_id        TEXT NOT NULL,
  location_id        TEXT NOT NULL,
  variant_id         TEXT NOT NULL,
  bucket             TEXT NOT NULL CHECK (bucket IN ('sellable','damaged')),
  on_hand_milli      INTEGER NOT NULL,
  last_movement_at   INTEGER NOT NULL,
  updated_at         INTEGER NOT NULL,
  PRIMARY KEY (location_id, variant_id, bucket)
) STRICT;
CREATE INDEX ix_balances_negative ON stock_balances(on_hand_milli) WHERE on_hand_milli < 0;  -- INV-008 flag view
```

Invariant (property test in `E11-T01`, and the `E11-T06` drift tool): for every
`(location, variant, bucket)`, `on_hand_milli = SUM(quantity_delta_milli)` over its movements.

---

## 0006_sales_tenders.sql

```sql
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
```

Invariants checked by `LocalStore::apply` in debug builds, and by property tests in `E09-T08`:

- `Σ sale_lines.total_minor + rounding_minor = total_minor`
- `Σ tenders.amount_minor = total_minor`
- `tendered_total_minor − change_minor = total_minor`

---

## 0007_returns.sql

```sql
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
```

RET-004 is enforced inside the `apply` transaction. The return use case re-reads
`SUM(return_lines.quantity_milli)` per original line **inside** `BEGIN IMMEDIATE`, through a
store precondition hook (`WriteSet::precondition(ReturnQtyAvailable{..})`). Two concurrent
returns against the same sale on one register therefore cannot race.

Two registers returning the same sale while both are offline *can* exceed the sold quantity.
The cloud detects this (`E17-T05`) and raises `return.over_returned` on the repair dashboard.

---

## 0008_receipts_printing.sql

```sql
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
```

---

## 0009_cash_sessions.sql (DER-001)

```sql
CREATE TABLE cash_sessions (
  cash_session_id      TEXT PRIMARY KEY,
  business_id          TEXT NOT NULL,
  register_id          TEXT NOT NULL REFERENCES registers(register_id),
  opened_by            TEXT NOT NULL,
  opened_at            INTEGER NOT NULL,
  opening_float_minor  INTEGER NOT NULL CHECK (opening_float_minor >= 0),
  closed_by            TEXT,
  closed_at            INTEGER,
  expected_cash_minor  INTEGER,       -- float + cash sales − cash refunds (computed at close)
  counted_cash_minor   INTEGER,
  variance_minor       INTEGER,
  business_date        TEXT NOT NULL, -- date the session opened
  status               TEXT NOT NULL CHECK (status IN ('open','closed')),
  note                 TEXT
) STRICT;
CREATE UNIQUE INDEX ux_one_open_session ON cash_sessions(register_id) WHERE status = 'open';
```

A cash session is a fact in two parts: `cash_session.opened` and `cash_session.closed` are
separate outbox events. Closing a session updates `closed_*` once. A trigger forbids any
update after `status = 'closed'`.

---

## 0010_import.sql (CAT-008)

```sql
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
```

---

## 0011_sync.sql (Stage 3)

```sql
CREATE TABLE sync_conflicts (                          -- SYN-010, AT-07
  conflict_id          TEXT PRIMARY KEY,
  aggregate_type       TEXT NOT NULL,
  aggregate_id         TEXT NOT NULL,
  outbox_event_id      TEXT NOT NULL REFERENCES outbox_events(event_id),
  local_payload_json   TEXT NOT NULL CHECK (json_valid(local_payload_json)),
  server_payload_json  TEXT NOT NULL CHECK (json_valid(server_payload_json)),
  local_base_version   INTEGER NOT NULL,
  server_version       INTEGER NOT NULL,
  status               TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open','resolved','superseded')),
  resolution_json      TEXT CHECK (resolution_json IS NULL OR json_valid(resolution_json)),
  resolved_by          TEXT,
  resolved_at          INTEGER,
  created_at           INTEGER NOT NULL
) STRICT;
CREATE INDEX ix_conflicts_open ON sync_conflicts(status, aggregate_type);

-- Inbound changes applied (diagnostics + idempotent re-apply guard).
CREATE TABLE inbound_changes (
  server_seq      INTEGER PRIMARY KEY,
  aggregate_type  TEXT NOT NULL,
  aggregate_id    TEXT NOT NULL,
  version         INTEGER,
  applied_at      INTEGER NOT NULL,
  outcome         TEXT NOT NULL CHECK (outcome IN ('applied','skipped_older','skipped_own','conflict_pending'))
) STRICT;
```

**Inbound application rule.** A pulled master-data change is applied when
`incoming.version > local.version` **and** the local aggregate has no unacknowledged outbox
event. If the local aggregate does have a pending edit, the change is held as
`conflict_pending`. When the push result arrives it settles the case: either the local edit
was accepted (and the incoming change is older), or it produced a conflict row.

Pulled facts from other registers are inserted with `INSERT … ON CONFLICT DO NOTHING`. Their
stock movements update `stock_balances`.

---

## Performance indexes and dataset targets (NFR rows of PRD §14)

| Query | Index | Target | Verified by |
|---|---|---|---|
| Barcode → variant | `ux_barcode_value` | p95 < 50 ms @ 100k aliases | E06-T04 bench, E19-T01 |
| SKU → variant | `ux_variant_sku` | p95 < 50 ms | E06-T04 |
| Name search | `product_search` FTS5 | p95 < 150 ms @ 50k variants | E06-T04 |
| Sales by day | `ix_sales_date` | Report < 1 s @ 500k lines | E14-T01, E19-T01 |
| Top products | `ix_sale_lines_variant` plus a covering query on date join | < 1.5 s @ 500k lines | E14-T02 |
| Pending outbox | `ix_outbox_pending` | < 10 ms | E17-T03 |

If the 500k-line report target fails, add a `daily_sales_summary` projection table that is
updated inside `apply` (`E14-T01` contingency) instead of adding caching layers.
