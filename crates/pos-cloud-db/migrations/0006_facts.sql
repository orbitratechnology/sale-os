-- 0006 (spec C0005): facts pushed by registers (E17-T05, SYN-009, PRD §11.1/§11.5). Facts
-- are insert-only for the application role: a sale, a return, a stock document or a cash
-- session is never changed after it happened. Only the projections (`stock_balances`,
-- `daily_sales_rollup`) are updated. Every table carries `business_id` and row-level
-- security. Each fact also keeps its full payload (`snapshot`), so nothing the register sent
-- is lost; the columns are what reports and stock need. Append-only once merged to main.

CREATE TABLE sales (
  business_id          uuid NOT NULL,
  sale_id              uuid NOT NULL,
  register_id          uuid NOT NULL,
  location_id          uuid NOT NULL,
  cashier              uuid NOT NULL,
  approver             uuid,
  mode                 text NOT NULL CHECK (mode IN ('sandbox','live')),
  receipt_seq          bigint NOT NULL,
  receipt_number       text NOT NULL,
  occurred_at          timestamptz NOT NULL,
  business_date        date NOT NULL,
  currency_code        text NOT NULL,
  subtotal_minor       bigint NOT NULL,
  discount_minor       bigint NOT NULL,
  tax_minor            bigint NOT NULL,
  rounding_minor       bigint NOT NULL,
  total_minor          bigint NOT NULL,
  origin_event_id      uuid NOT NULL,
  snapshot             jsonb NOT NULL,
  received_at          timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (business_id, sale_id),
  UNIQUE (business_id, register_id, receipt_seq),
  FOREIGN KEY (business_id, register_id) REFERENCES registers(business_id, register_id)
);
CREATE INDEX ix_sales_date ON sales(business_id, business_date);

CREATE TABLE sale_lines (
  business_id          uuid NOT NULL,
  line_id              uuid NOT NULL,
  sale_id              uuid NOT NULL,
  line_no              int NOT NULL,
  product_id           uuid NOT NULL,
  variant_id           uuid NOT NULL,
  name                 text NOT NULL,
  quantity_milli       bigint NOT NULL CHECK (quantity_milli > 0),
  unit_price_minor     bigint NOT NULL,
  gross_minor          bigint NOT NULL,
  discount_minor       bigint NOT NULL,
  tax_minor            bigint NOT NULL,
  total_minor          bigint NOT NULL,
  PRIMARY KEY (business_id, line_id),
  FOREIGN KEY (business_id, sale_id) REFERENCES sales(business_id, sale_id)
);
CREATE INDEX ix_sale_lines_sale ON sale_lines(business_id, sale_id);

CREATE TABLE tenders (
  business_id          uuid NOT NULL,
  tender_id            uuid NOT NULL,
  sale_id              uuid NOT NULL,
  seq                  int NOT NULL,
  kind                 text NOT NULL,
  amount_minor         bigint NOT NULL,
  status               text NOT NULL,
  PRIMARY KEY (business_id, tender_id),
  FOREIGN KEY (business_id, sale_id) REFERENCES sales(business_id, sale_id)
);

CREATE TABLE returns (
  business_id          uuid NOT NULL,
  return_id            uuid NOT NULL,
  original_sale_id     uuid NOT NULL,
  register_id          uuid NOT NULL,
  location_id          uuid NOT NULL,
  actor                uuid NOT NULL,
  receipt_number       text NOT NULL,
  reason               text NOT NULL,
  occurred_at          timestamptz NOT NULL,
  business_date        date NOT NULL,
  subtotal_minor       bigint NOT NULL,
  tax_minor            bigint NOT NULL,
  total_minor          bigint NOT NULL,
  origin_event_id      uuid NOT NULL,
  snapshot             jsonb NOT NULL,
  received_at          timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (business_id, return_id),
  FOREIGN KEY (business_id, original_sale_id) REFERENCES sales(business_id, sale_id),
  FOREIGN KEY (business_id, register_id) REFERENCES registers(business_id, register_id)
);

CREATE TABLE return_lines (
  business_id          uuid NOT NULL,
  return_line_id       uuid NOT NULL,
  return_id            uuid NOT NULL,
  sale_line_id         uuid NOT NULL,
  variant_id           uuid NOT NULL,
  quantity_milli       bigint NOT NULL CHECK (quantity_milli > 0),
  disposition          text NOT NULL,
  refund_minor         bigint NOT NULL,
  tax_minor            bigint NOT NULL,
  PRIMARY KEY (business_id, return_line_id),
  FOREIGN KEY (business_id, return_id) REFERENCES returns(business_id, return_id),
  FOREIGN KEY (business_id, sale_line_id) REFERENCES sale_lines(business_id, line_id)
);
CREATE INDEX ix_return_lines_sale_line ON return_lines(business_id, sale_line_id);

CREATE TABLE refunds (
  business_id          uuid NOT NULL,
  refund_id            uuid NOT NULL,
  return_id            uuid NOT NULL,
  seq                  int NOT NULL,
  kind                 text NOT NULL,
  amount_minor         bigint NOT NULL,
  status               text NOT NULL,
  PRIMARY KEY (business_id, refund_id),
  FOREIGN KEY (business_id, return_id) REFERENCES returns(business_id, return_id)
);

CREATE TABLE inventory_documents (
  business_id          uuid NOT NULL,
  document_id          uuid NOT NULL,
  doc_type             text NOT NULL,
  document_no          text NOT NULL,
  register_id          uuid NOT NULL,
  location_id          uuid NOT NULL,
  actor                uuid NOT NULL,
  reason               text,
  occurred_at          timestamptz NOT NULL,
  business_date        date NOT NULL,
  origin_event_id      uuid NOT NULL,
  snapshot             jsonb NOT NULL,
  received_at          timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (business_id, document_id),
  FOREIGN KEY (business_id, register_id) REFERENCES registers(business_id, register_id)
);

-- The ledger: every change of stock, from sales, returns and documents of every register.
CREATE TABLE stock_movements (
  business_id          uuid NOT NULL,
  movement_id          uuid NOT NULL,
  location_id          uuid NOT NULL,
  variant_id           uuid NOT NULL,
  bucket               text NOT NULL CHECK (bucket IN ('sellable','damaged')),
  movement_type        text NOT NULL,
  delta_milli          bigint NOT NULL CHECK (delta_milli <> 0),
  unit_cost_minor      bigint,
  source_type          text NOT NULL,
  source_id            uuid NOT NULL,
  register_id          uuid NOT NULL,
  occurred_at          timestamptz NOT NULL,
  business_date        date NOT NULL,
  PRIMARY KEY (business_id, movement_id)
);
CREATE INDEX ix_cloud_movements_variant ON stock_movements(business_id, location_id, variant_id);

-- Projection of the ledger, updated in the applier's transaction (§11.5: may go negative,
-- which is flagged, never refused).
CREATE TABLE stock_balances (
  business_id          uuid NOT NULL,
  location_id          uuid NOT NULL,
  variant_id           uuid NOT NULL,
  bucket               text NOT NULL CHECK (bucket IN ('sellable','damaged')),
  on_hand_milli        bigint NOT NULL,
  updated_at           timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (business_id, location_id, variant_id, bucket)
);

-- Cash sessions: the opening and the closing are separate facts, so neither is updated.
CREATE TABLE cash_sessions (
  business_id          uuid NOT NULL,
  cash_session_id      uuid NOT NULL,
  register_id          uuid NOT NULL,
  location_id          uuid NOT NULL,
  opened_by            uuid NOT NULL,
  opened_at            timestamptz NOT NULL,
  business_date        date NOT NULL,
  opening_float_minor  bigint NOT NULL,
  origin_event_id      uuid NOT NULL,
  PRIMARY KEY (business_id, cash_session_id),
  FOREIGN KEY (business_id, register_id) REFERENCES registers(business_id, register_id)
);

CREATE TABLE cash_session_closings (
  business_id          uuid NOT NULL,
  cash_session_id      uuid NOT NULL,
  register_id          uuid NOT NULL,
  session_number       text NOT NULL,
  closed_by            uuid NOT NULL,
  closed_at            timestamptz NOT NULL,
  expected_minor       bigint NOT NULL,
  counted_minor        bigint NOT NULL,
  variance_minor       bigint NOT NULL,
  totals               jsonb NOT NULL,
  note                 text,
  origin_event_id      uuid NOT NULL,
  PRIMARY KEY (business_id, cash_session_id),
  FOREIGN KEY (business_id, cash_session_id) REFERENCES cash_sessions(business_id, cash_session_id)
);

-- Standalone audit entries (login failures, approvals, reprints, exports...).
CREATE TABLE audit_events (
  business_id          uuid NOT NULL,
  audit_id             uuid NOT NULL,
  register_id          uuid NOT NULL,
  action               text NOT NULL,
  entity_type          text NOT NULL,
  entity_id            text NOT NULL,
  metadata             jsonb NOT NULL,
  occurred_at          timestamptz NOT NULL,
  origin_event_id      uuid NOT NULL,
  PRIMARY KEY (business_id, audit_id)
);

-- Per day and register, live sales only (sandbox sales are kept but not counted).
CREATE TABLE daily_sales_rollup (
  business_id          uuid NOT NULL,
  business_date        date NOT NULL,
  register_id          uuid NOT NULL,
  sales_count          int NOT NULL DEFAULT 0,
  sales_total_minor    bigint NOT NULL DEFAULT 0,
  tax_minor            bigint NOT NULL DEFAULT 0,
  discount_minor       bigint NOT NULL DEFAULT 0,
  returns_count        int NOT NULL DEFAULT 0,
  refunds_total_minor  bigint NOT NULL DEFAULT 0,
  PRIMARY KEY (business_id, business_date, register_id)
);

-- Things that happened offline and need an owner's eye: stock gone negative across
-- registers, more returned than was sold (§11.5). Accepted facts, flagged.
CREATE TABLE sync_flags (
  business_id          uuid NOT NULL,
  flag_id              bigserial,
  kind                 text NOT NULL CHECK (kind IN ('negative_stock','over_return')),
  subject_id           uuid NOT NULL,
  detail               jsonb NOT NULL,
  origin_event_id      uuid NOT NULL,
  created_at           timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (business_id, flag_id)
);

GRANT SELECT, INSERT ON sales, sale_lines, tenders, returns, return_lines, refunds, inventory_documents,
  stock_movements, cash_sessions, cash_session_closings, audit_events, sync_flags TO pos_app;
GRANT SELECT, INSERT, UPDATE ON stock_balances, daily_sales_rollup TO pos_app;
GRANT USAGE ON SEQUENCE sync_flags_flag_id_seq TO pos_app;

DO $$
DECLARE t text;
BEGIN
  FOREACH t IN ARRAY ARRAY['sales', 'sale_lines', 'tenders', 'returns', 'return_lines', 'refunds',
      'inventory_documents', 'stock_movements', 'stock_balances', 'cash_sessions', 'cash_session_closings',
      'audit_events', 'daily_sales_rollup', 'sync_flags'] LOOP
    EXECUTE format('ALTER TABLE %I ENABLE ROW LEVEL SECURITY', t);
    EXECUTE format('ALTER TABLE %I FORCE ROW LEVEL SECURITY', t);
    EXECUTE format(
      'CREATE POLICY tenant_isolation ON %I
         USING      (business_id = nullif(current_setting(''app.business_id'', true), '''')::uuid)
         WITH CHECK (business_id = nullif(current_setting(''app.business_id'', true), '''')::uuid)', t);
  END LOOP;
END $$;
