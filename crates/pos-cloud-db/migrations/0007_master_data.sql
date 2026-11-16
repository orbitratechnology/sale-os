-- 0007 (spec C0004): master data as the registers edit it (E17-T06, SYN-010, ADR-011). Each
-- aggregate has a `version`; an edit applies only on top of the version it was based on.
-- Each row keeps the aggregate's last snapshot (what other registers pull, E17-T07) beside
-- the columns the cloud queries. Uniqueness that registers can break while offline (an
-- active barcode, an active SKU) is enforced here and becomes a conflict for the second
-- register. Append-only once merged to main.

CREATE TABLE staff_users (
  business_id   uuid NOT NULL,
  user_id       uuid NOT NULL,
  display_name  text NOT NULL,
  role          text NOT NULL,
  pin_hash      text NOT NULL,              -- Argon2id PHC; the PIN never leaves a register
  status        text NOT NULL CHECK (status IN ('active','inactive')),
  version       bigint NOT NULL,
  snapshot      jsonb NOT NULL,
  updated_at    timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (business_id, user_id)
);

CREATE TABLE business_settings (
  business_id   uuid NOT NULL,
  key           text NOT NULL,
  value         jsonb NOT NULL,
  version       bigint NOT NULL,
  snapshot      jsonb NOT NULL,
  updated_at    timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (business_id, key)
);

CREATE TABLE tax_rates (
  business_id     uuid NOT NULL,
  tax_rate_id     uuid NOT NULL,
  name            text NOT NULL,
  rate_ppm        int NOT NULL CHECK (rate_ppm BETWEEN 0 AND 1000000),
  effective_from  date,
  effective_to    date,
  status          text NOT NULL CHECK (status IN ('active','archived')),
  version         bigint NOT NULL,
  snapshot        jsonb NOT NULL,
  updated_at      timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (business_id, tax_rate_id)
);

CREATE TABLE tax_categories (
  business_id      uuid NOT NULL,
  tax_category_id  uuid NOT NULL,
  name             text NOT NULL,
  is_default       boolean NOT NULL,
  rate_ids         uuid[] NOT NULL,
  status           text NOT NULL CHECK (status IN ('active','archived')),
  version          bigint NOT NULL,
  snapshot         jsonb NOT NULL,
  updated_at       timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (business_id, tax_category_id)
);

CREATE TABLE categories (
  business_id   uuid NOT NULL,
  category_id   uuid NOT NULL,
  parent_id     uuid,
  name          text NOT NULL,
  sort_order    bigint NOT NULL,
  status        text NOT NULL CHECK (status IN ('active','archived')),
  version       bigint NOT NULL,
  snapshot      jsonb NOT NULL,
  updated_at    timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (business_id, category_id)
);

CREATE TABLE products (
  business_id      uuid NOT NULL,
  product_id       uuid NOT NULL,
  name             text NOT NULL,
  description      text,
  category_id      uuid,
  product_type     text NOT NULL,
  unit_code        text NOT NULL,
  tax_category_id  uuid NOT NULL,
  quick_select     boolean NOT NULL,
  status           text NOT NULL CHECK (status IN ('active','archived')),
  version          bigint NOT NULL,
  snapshot         jsonb NOT NULL,
  updated_at       timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (business_id, product_id)
);

CREATE TABLE product_variants (
  business_id                uuid NOT NULL,
  variant_id                 uuid NOT NULL,
  product_id                 uuid NOT NULL,
  is_default                 boolean NOT NULL,
  sku                        text,
  options                    jsonb NOT NULL,
  price_minor                bigint NOT NULL,
  cost_minor                 bigint,
  track_stock                boolean NOT NULL,
  low_stock_threshold_milli  bigint,
  scan_qty_milli             bigint NOT NULL,
  status                     text NOT NULL CHECK (status IN ('active','archived')),
  PRIMARY KEY (business_id, variant_id),
  FOREIGN KEY (business_id, product_id) REFERENCES products(business_id, product_id)
);
CREATE UNIQUE INDEX ux_cloud_variant_sku ON product_variants(business_id, lower(sku))
  WHERE sku IS NOT NULL AND status = 'active';

-- A product's barcodes are replaced as a set on every upsert.
CREATE TABLE product_barcodes (
  business_id       uuid NOT NULL,
  barcode_id        uuid NOT NULL,
  variant_id        uuid NOT NULL,
  product_id        uuid NOT NULL,
  value_normalized  text NOT NULL,
  value_raw         text NOT NULL,
  symbology         text NOT NULL,
  qualifier         text NOT NULL,
  pack_qty_milli    bigint NOT NULL,
  status            text NOT NULL CHECK (status IN ('active','archived')),
  PRIMARY KEY (business_id, barcode_id),
  FOREIGN KEY (business_id, variant_id) REFERENCES product_variants(business_id, variant_id)
);
CREATE UNIQUE INDEX ux_cloud_barcode ON product_barcodes(business_id, value_normalized) WHERE status = 'active';
CREATE INDEX ix_cloud_barcodes_product ON product_barcodes(business_id, product_id);

-- The rows the link flow created (version 0) get the register's own snapshot on its first
-- push; afterwards they move like any other aggregate.
ALTER TABLE businesses ADD COLUMN snapshot jsonb;
ALTER TABLE locations ADD COLUMN snapshot jsonb;
ALTER TABLE registers ADD COLUMN snapshot jsonb;

GRANT SELECT, INSERT, UPDATE ON staff_users, business_settings, tax_rates, tax_categories, categories, products,
  product_variants, product_barcodes TO pos_app;
GRANT DELETE ON product_barcodes TO pos_app;

DO $$
DECLARE t text;
BEGIN
  FOREACH t IN ARRAY ARRAY['staff_users', 'business_settings', 'tax_rates', 'tax_categories', 'categories',
      'products', 'product_variants', 'product_barcodes'] LOOP
    EXECUTE format('ALTER TABLE %I ENABLE ROW LEVEL SECURITY', t);
    EXECUTE format('ALTER TABLE %I FORCE ROW LEVEL SECURITY', t);
    EXECUTE format(
      'CREATE POLICY tenant_isolation ON %I
         USING      (business_id = nullif(current_setting(''app.business_id'', true), '''')::uuid)
         WITH CHECK (business_id = nullif(current_setting(''app.business_id'', true), '''')::uuid)', t);
  END LOOP;
END $$;
