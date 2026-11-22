-- 0023_scales: a till's devices can include a scale (E20-T06). SQLite cannot widen a CHECK
-- constraint in place, so `devices` is rebuilt with the same rows. Nothing references it.
-- Append-only: once merged to main this file is never edited.

CREATE TABLE devices_new (
  device_id    TEXT PRIMARY KEY,
  register_id  TEXT NOT NULL REFERENCES registers(register_id),
  kind         TEXT NOT NULL CHECK (kind IN ('receipt_printer','scanner','payment_terminal','customer_display','scale')),
  role         TEXT NOT NULL DEFAULT 'main' CHECK (length(role) BETWEEN 1 AND 32),
  name         TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 80),
  config_json  TEXT NOT NULL CHECK (json_valid(config_json)),
  enabled      INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0,1)),
  version      INTEGER NOT NULL DEFAULT 1,
  created_at   INTEGER NOT NULL,
  updated_at   INTEGER NOT NULL,
  UNIQUE (register_id, kind, role)
) STRICT;

INSERT INTO devices_new (device_id, register_id, kind, role, name, config_json, enabled, version, created_at, updated_at)
  SELECT device_id, register_id, kind, role, name, config_json, enabled, version, created_at, updated_at FROM devices;
DROP TABLE devices;
ALTER TABLE devices_new RENAME TO devices;
