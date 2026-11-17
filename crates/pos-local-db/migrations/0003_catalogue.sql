-- 0003_catalogue: units, tax rates and categories, product categories, products, variants,
-- barcodes and the FTS5 search index (E06-T01, CAT-002..007, ADR-015, ADR-019).
-- Spec: docs/backlog/04-local-database.md#0003_cataloguesql
-- Append-only: once merged to main this file is never edited.

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
