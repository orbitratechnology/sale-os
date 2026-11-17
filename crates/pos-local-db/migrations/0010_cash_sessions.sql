-- 0010: cash sessions (E15-T02, DER-001). Spec: docs/backlog/04-local-database.md#0010_cash_sessionssql
-- Append-only: once merged to main this file is never edited.

CREATE TABLE cash_sessions (
  cash_session_id      TEXT PRIMARY KEY,
  business_id          TEXT NOT NULL,
  register_id          TEXT NOT NULL REFERENCES registers(register_id),
  session_seq          INTEGER NOT NULL CHECK (session_seq > 0),
  session_number       TEXT NOT NULL,                 -- '{register_code}-Z-{seq:04}', printed on the Z summary
  opened_by            TEXT NOT NULL,
  opened_at            INTEGER NOT NULL,
  opening_float_minor  INTEGER NOT NULL CHECK (opening_float_minor >= 0),
  business_date        TEXT NOT NULL,                 -- the date the session opened
  status               TEXT NOT NULL CHECK (status IN ('open','closed')),
  closed_by            TEXT,
  closed_at            INTEGER,
  expected_cash_minor  INTEGER,                       -- float + cash kept from sales − cash refunds, at close
  counted_cash_minor   INTEGER CHECK (counted_cash_minor IS NULL OR counted_cash_minor >= 0),
  variance_minor       INTEGER,                       -- counted − expected
  totals_json          TEXT CHECK (totals_json IS NULL OR json_valid(totals_json)),  -- SessionTotals at close
  note                 TEXT,
  CHECK ((status = 'open' AND closed_at IS NULL AND counted_cash_minor IS NULL)
      OR (status = 'closed' AND closed_at IS NOT NULL AND closed_by IS NOT NULL AND counted_cash_minor IS NOT NULL
          AND expected_cash_minor IS NOT NULL AND variance_minor IS NOT NULL AND totals_json IS NOT NULL)),
  UNIQUE (register_id, session_seq)
) STRICT;
CREATE UNIQUE INDEX ux_one_open_session ON cash_sessions(register_id) WHERE status = 'open';

-- Closing is the only change, and it happens once.
CREATE TRIGGER cash_sessions_closed_is_final BEFORE UPDATE ON cash_sessions WHEN OLD.status = 'closed'
BEGIN SELECT RAISE(ABORT, 'a closed cash session is final'); END;
CREATE TRIGGER cash_sessions_opening_is_fixed BEFORE UPDATE OF cash_session_id, business_id, register_id, session_seq,
  session_number, opened_by, opened_at, opening_float_minor, business_date ON cash_sessions
BEGIN SELECT RAISE(ABORT, 'the opening of a cash session cannot change'); END;
CREATE TRIGGER cash_sessions_no_delete BEFORE DELETE ON cash_sessions
BEGIN SELECT RAISE(ABORT, 'cash sessions are never deleted'); END;

-- Sales (column since 0006) and returns belong to the session open when they were made.
ALTER TABLE returns ADD COLUMN cash_session_id TEXT REFERENCES cash_sessions(cash_session_id);
CREATE INDEX ix_sales_cash_session ON sales(cash_session_id) WHERE cash_session_id IS NOT NULL;
CREATE INDEX ix_returns_cash_session ON returns(cash_session_id) WHERE cash_session_id IS NOT NULL;
