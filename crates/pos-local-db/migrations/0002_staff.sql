-- 0002_staff: staff identities, device-local login throttling and sessions (E05-T01/T02).
-- Spec: docs/backlog/04-local-database.md#0002_staffsql (ADR-013).
-- Append-only: once merged to main this file is never edited.

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
