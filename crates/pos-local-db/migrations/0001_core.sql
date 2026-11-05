-- 0001_core: business, location, register identity, settings, counters, audit log,
-- transactional outbox and sync state.
-- Spec: docs/backlog/04-local-database.md#0001_coresql (ADR-005..009).
-- Append-only: once merged to main this file is never edited; fixes go in a new migration.

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
