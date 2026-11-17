-- 0004_carts: draft carts and lines (local only, never synced, ADR-009), plus the scan
-- de-duplication window (BAR-007). E09-T01 / E07-T02.
-- Spec: docs/backlog/04-local-database.md#0004_cartssql
-- Append-only: once merged to main this file is never edited.

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
