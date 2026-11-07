-- 0005_inventory: the stock ledger (append-only movements) and its balance projection.
-- E11-T01, INV-001..004, ADR-020. Spec: docs/backlog/04-local-database.md#0005_inventorysql
-- Append-only: once merged to main this file is never edited.

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
