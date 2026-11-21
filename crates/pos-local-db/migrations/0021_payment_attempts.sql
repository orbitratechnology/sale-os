-- 0021_payment_attempts: payment terminal attempts and their history.
-- E10-T02, PAY-004..006, PRD §6.5 and §12.5.
-- Append-only: once merged to main this file is never edited.
--
-- Local only (ADR-009): an attempt is working state of a cart, like the cart itself. The
-- money outcome that matters travels in the completed sale's tender (`sale.completed`,
-- confirmation = provider, external_reference = the provider reference).
--
-- PAY-005: no card number, CVV, PIN or track data, ever. `masked_pan` holds the last four
-- digits only, and the database refuses anything else.

CREATE TABLE payment_attempts (
  attempt_id       TEXT PRIMARY KEY,                -- also the idempotency key sent to the provider
  business_id      TEXT NOT NULL,
  register_id      TEXT NOT NULL,
  cart_id          TEXT NOT NULL,
  kind             TEXT NOT NULL CHECK (kind IN ('sale','refund')),
  provider         TEXT NOT NULL,                   -- the connector, e.g. 'mock'
  amount_minor     INTEGER NOT NULL CHECK (amount_minor > 0),
  currency_code    TEXT NOT NULL CHECK (length(currency_code) = 3),
  status           TEXT NOT NULL CHECK (status IN ('pending','approved','declined','cancelled','reversed','reconciliation_required')),
  approval_source  TEXT CHECK (approval_source IN ('provider','operator')),  -- who said it went through
  provider_ref     TEXT CHECK (provider_ref IS NULL OR length(provider_ref) BETWEEN 1 AND 64),
  auth_code        TEXT CHECK (auth_code IS NULL OR length(auth_code) BETWEEN 1 AND 64),
  card_brand       TEXT CHECK (card_brand IS NULL OR length(card_brand) BETWEEN 1 AND 32),
  masked_pan       TEXT CHECK (masked_pan IS NULL OR masked_pan GLOB '[*][*][*][*][0-9][0-9][0-9][0-9]'),
  status_reason    TEXT CHECK (status_reason IS NULL OR length(status_reason) <= 200),
  tender_id        TEXT REFERENCES tenders(tender_id),  -- set once, by the completed sale it paid for
  created_by       TEXT NOT NULL,
  created_at       INTEGER NOT NULL,
  updated_at       INTEGER NOT NULL,
  CHECK ((status IN ('approved','reversed')) = (approval_source IS NOT NULL)),
  CHECK (approval_source IS NOT 'provider' OR provider_ref IS NOT NULL),
  CHECK (tender_id IS NULL OR (status = 'approved' AND kind = 'sale'))
) STRICT;
CREATE INDEX ix_payment_attempts_cart ON payment_attempts(cart_id, status);
CREATE UNIQUE INDEX ux_payment_attempts_tender ON payment_attempts(tender_id) WHERE tender_id IS NOT NULL;
-- One open outcome per cart: a second payment cannot start while the first may still charge.
CREATE UNIQUE INDEX ux_payment_attempts_one_open ON payment_attempts(cart_id)
  WHERE status IN ('pending','reconciliation_required');

CREATE TRIGGER payment_attempts_no_delete BEFORE DELETE ON payment_attempts
BEGIN SELECT RAISE(ABORT, 'payment attempts are kept'); END;
CREATE TRIGGER payment_attempts_fixed_fields
BEFORE UPDATE OF attempt_id, business_id, register_id, cart_id, kind, provider, amount_minor, currency_code,
  created_by, created_at ON payment_attempts
BEGIN SELECT RAISE(ABORT, 'only a payment attempt''s outcome may change'); END;
CREATE TRIGGER payment_attempts_final BEFORE UPDATE ON payment_attempts
WHEN OLD.status IN ('declined','cancelled','reversed') OR OLD.tender_id IS NOT NULL
BEGIN SELECT RAISE(ABORT, 'this payment attempt is final'); END;

CREATE TABLE payment_attempt_events (                -- append-only history; payment_attempts.status is the projection
  attempt_id     TEXT NOT NULL REFERENCES payment_attempts(attempt_id),
  seq            INTEGER NOT NULL CHECK (seq > 0),
  from_status    TEXT,
  to_status      TEXT NOT NULL CHECK (to_status IN ('pending','approved','declined','cancelled','reversed','reconciliation_required')),
  source         TEXT NOT NULL CHECK (source IN ('operator','provider','system')),
  detail_json    TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(detail_json)),
  actor_user_id  TEXT,
  occurred_at    INTEGER NOT NULL,
  PRIMARY KEY (attempt_id, seq)
) STRICT;

CREATE TRIGGER payment_attempt_events_no_update BEFORE UPDATE ON payment_attempt_events
BEGIN SELECT RAISE(ABORT, 'append-only'); END;
CREATE TRIGGER payment_attempt_events_no_delete BEFORE DELETE ON payment_attempt_events
BEGIN SELECT RAISE(ABORT, 'append-only'); END;
