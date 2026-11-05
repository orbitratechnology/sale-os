# 05 — Cloud Database (PostgreSQL): Tenancy, Inbox, Change Log, Projections

Migrations live in `crates/pos-cloud-db/migrations/`. They are applied by
`cloud-api migrate`, which runs as the **owner** role. The service itself connects as
`pos_app`, a role that does not own the tables and does not have `BYPASSRLS` (ADR-016).

Migration order:

| # | File | Task |
|---|---|---|
| C0001 | `identity_tenancy.sql` | E16-T02 |
| C0002 | `devices_credentials.sql` | E16-T04 |
| C0003 | `sync_inbox_changelog.sql` | E17-T04 |
| C0004 | `master_data.sql` | E17-T06 |
| C0005 | `facts.sql` | E17-T05 |
| C0006 | `reporting.sql` | E14-T05 |
| C0007 | `rls_policies.sql` | E16-T05 (policy per table; every later migration adds its own policy) |

---

## C0001 identity_tenancy.sql

```sql
CREATE EXTENSION IF NOT EXISTS citext;

-- Global (non-tenant) tables: no RLS; accessed only via auth service code paths.
CREATE TABLE accounts (                 -- cloud owner identities (D-09: replaceable by OIDC)
  account_id     uuid PRIMARY KEY,
  email          citext NOT NULL UNIQUE,
  password_hash  text,                  -- argon2id PHC; NULL when using external IdP
  email_verified boolean NOT NULL DEFAULT false,
  status         text NOT NULL DEFAULT 'active' CHECK (status IN ('active','locked','deleted')),
  created_at     timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE businesses (
  business_id        uuid PRIMARY KEY,          -- client-generated (ADR-005, ADR-009)
  name               text NOT NULL,
  country_code       char(2) NOT NULL,
  timezone           text NOT NULL,
  currency_code      char(3) NOT NULL,
  currency_exponent  smallint NOT NULL CHECK (currency_exponent BETWEEN 0 AND 4),
  prices_include_tax boolean NOT NULL,
  profile            jsonb NOT NULL DEFAULT '{}',
  version            bigint NOT NULL DEFAULT 1,
  status             text NOT NULL DEFAULT 'active' CHECK (status IN ('active','suspended','deleted')),
  created_at         timestamptz NOT NULL DEFAULT now(),
  updated_at         timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE business_members (
  business_id  uuid NOT NULL REFERENCES businesses(business_id),
  account_id   uuid NOT NULL REFERENCES accounts(account_id),
  role         text NOT NULL CHECK (role IN ('owner')),   -- MVP: owners only in the cloud
  created_at   timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (business_id, account_id)
);

CREATE TABLE locations (
  business_id  uuid NOT NULL REFERENCES businesses(business_id),
  location_id  uuid NOT NULL,
  name         text NOT NULL,
  address      jsonb NOT NULL DEFAULT '{}',
  status       text NOT NULL,
  version      bigint NOT NULL,
  updated_at   timestamptz NOT NULL,
  PRIMARY KEY (business_id, location_id)
);

CREATE TABLE registers (
  business_id  uuid NOT NULL,
  register_id  uuid NOT NULL,
  location_id  uuid NOT NULL,
  name         text NOT NULL,
  code         text NOT NULL,
  receipt_prefix text NOT NULL DEFAULT '',
  status       text NOT NULL,
  version      bigint NOT NULL,
  updated_at   timestamptz NOT NULL,
  PRIMARY KEY (business_id, register_id),
  UNIQUE (business_id, code),
  FOREIGN KEY (business_id, location_id) REFERENCES locations(business_id, location_id)
);
```

## C0002 devices_credentials.sql

```sql
CREATE TABLE devices (
  business_id       uuid NOT NULL,
  device_id         uuid NOT NULL,
  register_id       uuid NOT NULL,
  installation_id   uuid NOT NULL,
  app_version       text NOT NULL,
  os_info           text,
  status            text NOT NULL DEFAULT 'active' CHECK (status IN ('active','revoked')),
  registered_by     uuid NOT NULL REFERENCES accounts(account_id),
  registered_at     timestamptz NOT NULL DEFAULT now(),
  last_seen_at      timestamptz,
  revoked_at        timestamptz,
  PRIMARY KEY (business_id, device_id),
  FOREIGN KEY (business_id, register_id) REFERENCES registers(business_id, register_id)
);
CREATE UNIQUE INDEX ux_active_device_per_register ON devices(business_id, register_id) WHERE status = 'active';

CREATE TABLE device_refresh_tokens (     -- rotation family (ADR-013)
  token_hash     bytea PRIMARY KEY,       -- SHA-256 of opaque token
  business_id    uuid NOT NULL,
  device_id      uuid NOT NULL,
  family_id      uuid NOT NULL,
  issued_at      timestamptz NOT NULL DEFAULT now(),
  expires_at     timestamptz NOT NULL,
  used_at        timestamptz,            -- reuse of a used token ⇒ revoke family + device
  FOREIGN KEY (business_id, device_id) REFERENCES devices(business_id, device_id)
);
```

## C0003 sync_inbox_changelog.sql

```sql
CREATE TABLE event_inbox (                -- idempotency + ack record (SYN-005/006, ADR-010)
  business_id      uuid NOT NULL,
  event_id         uuid NOT NULL,
  register_id      uuid NOT NULL,
  local_sequence   bigint NOT NULL,
  event_type       text NOT NULL,
  schema_version   int NOT NULL,
  occurred_at      timestamptz NOT NULL,
  received_at      timestamptz NOT NULL DEFAULT now(),
  payload          jsonb NOT NULL,
  payload_sha256   bytea NOT NULL,         -- same event_id with different payload ⇒ 'rejected: EVENT_ID_REUSED'
  result_status    text NOT NULL CHECK (result_status IN ('applied','conflict','rejected')),
  result           jsonb NOT NULL,          -- returned verbatim for duplicates
  PRIMARY KEY (business_id, event_id),
  UNIQUE (business_id, register_id, local_sequence)
);

CREATE TABLE register_sync_state (
  business_id        uuid NOT NULL,
  register_id        uuid NOT NULL,
  ack_sequence       bigint NOT NULL DEFAULT 0,
  last_push_at       timestamptz,
  last_pull_cursor   bigint,
  last_clock_skew_ms int,
  PRIMARY KEY (business_id, register_id)
);

-- Monotonic change feed for pulls (SYN-008). One row per accepted change of master data
-- and per fact (facts are pulled by other registers for stock/visibility).
CREATE TABLE change_log (
  server_seq        bigserial PRIMARY KEY,
  business_id       uuid NOT NULL,
  aggregate_type    text NOT NULL,
  aggregate_id      uuid NOT NULL,
  version           bigint,               -- master data only
  change_kind       text NOT NULL CHECK (change_kind IN ('upsert','archive','fact')),
  origin_register   uuid,                 -- NULL when changed by cloud admin
  origin_event_id   uuid,
  snapshot          jsonb NOT NULL,       -- full aggregate snapshot (master) or fact payload
  created_at        timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX ix_change_log_business_seq ON change_log(business_id, server_seq);
```

**Cursor semantics.** The pull cursor is the opaque string `"v1:<server_seq>"`.
`server_seq` comes from a `bigserial`, so it can have gaps but never goes backwards. Rows are
written in the same transaction as the change they describe.

Pulls must not skip a row whose transaction committed late. To prevent that, pulls only return
rows with `server_seq < (SELECT min in-flight seq)`. The **E17-T07 spike decides** between two
ways of doing this:

- (a) a single serialized appender per business, using a `pg_advisory_xact_lock` on the
  `business_id` hash, or
- (b) `pg_snapshot_xmin` visibility filtering.

The default is (a). It is simple, and the throughput per business is low.

## C0004 master_data.sql

These tables mirror the local catalogue with the cloud types. The primary key is
`(business_id, id)`, and every foreign key is composite.

```sql
CREATE TABLE staff_users (business_id uuid, user_id uuid, display_name text NOT NULL, role text NOT NULL,
  pin_hash text NOT NULL, status text NOT NULL, version bigint NOT NULL, updated_at timestamptz NOT NULL,
  PRIMARY KEY (business_id, user_id));
CREATE TABLE business_settings (business_id uuid, key text, value jsonb NOT NULL, version bigint NOT NULL,
  updated_at timestamptz NOT NULL, PRIMARY KEY (business_id, key));
CREATE TABLE tax_rates (business_id uuid, tax_rate_id uuid, name text NOT NULL, rate_ppm int NOT NULL CHECK (rate_ppm BETWEEN 0 AND 1000000),
  effective_from date, effective_to date, status text NOT NULL, version bigint NOT NULL, updated_at timestamptz NOT NULL,
  PRIMARY KEY (business_id, tax_rate_id));
CREATE TABLE tax_categories (business_id uuid, tax_category_id uuid, name text NOT NULL, is_default boolean NOT NULL,
  rate_ids uuid[] NOT NULL, status text NOT NULL, version bigint NOT NULL, updated_at timestamptz NOT NULL,
  PRIMARY KEY (business_id, tax_category_id));
CREATE TABLE categories (business_id uuid, category_id uuid, parent_id uuid, name text NOT NULL, sort_order int NOT NULL,
  status text NOT NULL, version bigint NOT NULL, updated_at timestamptz NOT NULL, PRIMARY KEY (business_id, category_id));
CREATE TABLE products (business_id uuid, product_id uuid, name text NOT NULL, description text, category_id uuid,
  product_type text NOT NULL, unit_code text NOT NULL, tax_category_id uuid NOT NULL, quick_select boolean NOT NULL,
  status text NOT NULL, version bigint NOT NULL, updated_at timestamptz NOT NULL, PRIMARY KEY (business_id, product_id));
CREATE TABLE product_variants (business_id uuid, variant_id uuid, product_id uuid NOT NULL, is_default boolean NOT NULL,
  sku text, options jsonb NOT NULL, label text NOT NULL, price_minor bigint NOT NULL, cost_minor bigint,
  track_stock boolean NOT NULL, low_stock_threshold_milli bigint, scan_qty_milli bigint NOT NULL, sort_order int NOT NULL,
  status text NOT NULL, PRIMARY KEY (business_id, variant_id),
  FOREIGN KEY (business_id, product_id) REFERENCES products(business_id, product_id));
CREATE UNIQUE INDEX ux_cloud_variant_sku ON product_variants(business_id, lower(sku)) WHERE sku IS NOT NULL AND status = 'active';
CREATE TABLE product_barcodes (business_id uuid, barcode_id uuid, variant_id uuid NOT NULL, value_normalized text NOT NULL,
  value_raw text NOT NULL, symbology text NOT NULL, qualifier text NOT NULL, pack_qty_milli bigint NOT NULL, status text NOT NULL,
  PRIMARY KEY (business_id, barcode_id),
  FOREIGN KEY (business_id, variant_id) REFERENCES product_variants(business_id, variant_id));
CREATE UNIQUE INDEX ux_cloud_barcode ON product_barcodes(business_id, value_normalized) WHERE status = 'active';
```

**Cross-register uniqueness collisions.** Two registers can each create the same barcode or
SKU while offline. The second `product.upserted` that arrives then breaks `ux_cloud_barcode`.

The applier catches the unique violation and returns `conflict` with the reason
`BARCODE_TAKEN`, together with the owning product snapshot. The local conflict UI offers two
fixes: reassign the barcode, or merge.

## C0005 facts.sql

These tables mirror the local `sales`, `sale_lines`, `tenders`, `tender_status_events`,
`returns`, `return_lines`, `refunds`, `inventory_documents`, `stock_movements`,
`cash_sessions` and `audit_events`, with these differences:

- `business_id uuid` is the leading column of every primary key and foreign key.
- Money and quantity columns are `bigint`. JSON columns are `jsonb`.
- `sales.mode` is kept, so sandbox sales can be excluded from reports and from `change_log`
  fan-out.
- `stock_balances (business_id, location_id, variant_id, bucket, on_hand_milli, updated_at)`
  is updated in the applier transaction.
- Every fact table has `source_event_id uuid NOT NULL`, pointing back to `event_inbox`.

Fact tables are append-only. The `pos_app` role gets `INSERT` and `SELECT` only; it is not
granted `UPDATE` or `DELETE`. The exception is `tenders.status`, which gets a column-level
`UPDATE` grant.

## C0006 reporting.sql

```sql
CREATE TABLE daily_sales_rollup (       -- maintained by the applier in the same txn (REP-008, §10.1)
  business_id     uuid NOT NULL,
  location_id     uuid NOT NULL,
  register_id     uuid NOT NULL,
  business_date   date NOT NULL,
  sale_count      int NOT NULL,
  gross_minor     bigint NOT NULL,
  discount_minor  bigint NOT NULL,
  tax_minor       bigint NOT NULL,
  refund_minor    bigint NOT NULL,
  net_minor       bigint NOT NULL,
  tender_totals   jsonb NOT NULL,         -- {"cash": n, "card_external": n, ...}
  PRIMARY KEY (business_id, location_id, register_id, business_date)
);
```

## C0007 rls_policies.sql (pattern, applied to every tenant table)

```sql
ALTER TABLE products ENABLE ROW LEVEL SECURITY;
ALTER TABLE products FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON products
  USING      (business_id = current_setting('app.business_id', true)::uuid)
  WITH CHECK (business_id = current_setting('app.business_id', true)::uuid);
```

- A missing setting yields `NULL`, which matches no rows. The system **fails closed**.
- `pos-cloud-db::TenantTx::begin(pool, principal)` runs
  `SET LOCAL app.business_id = $1` as the first statement. Repositories accept only a
  `TenantTx`, never a bare pool, so the setting cannot be skipped.
- The test `E16-T05` creates two businesses and calls every repository method and every route
  with business A's principal against business B's IDs. Each call must return "not found" or
  an empty result (AT-12).
