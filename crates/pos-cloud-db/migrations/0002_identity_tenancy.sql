-- 0002 (spec C0001): owner accounts, businesses and their locations and registers
-- (E16-T02, docs/backlog/05-cloud-database.md). Applied by the owner role; the service runs
-- as `pos_app` (no BYPASSRLS, owns nothing). Append-only once merged to main.
CREATE EXTENSION IF NOT EXISTS citext;

DO $$
BEGIN
  IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'pos_app') THEN
    -- The runtime role. Login roles used by the service are granted membership in it;
    -- `TenantTx` switches to it, so row-level security always applies.
    CREATE ROLE pos_app NOLOGIN NOSUPERUSER NOBYPASSRLS NOCREATEDB NOCREATEROLE;
  END IF;
END $$;

-- Global (non-tenant) table: no RLS; used only by the authentication code paths.
CREATE TABLE accounts (                 -- cloud owner identities (D-09: replaceable by OIDC)
  account_id     uuid PRIMARY KEY,
  email          citext NOT NULL UNIQUE,
  password_hash  text,                  -- argon2id PHC; NULL when using an external IdP
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
CREATE INDEX ix_business_members_account ON business_members(account_id);

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

GRANT SELECT, INSERT, UPDATE ON accounts, businesses, business_members, locations, registers TO pos_app;
