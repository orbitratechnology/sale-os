-- 0005 (spec C0003): the sync inbox, per-register ack state and the change feed (E17-T04,
-- ADR-010). Append-only once merged to main.

-- Idempotency and ack record (SYN-005/006): one row per handled event. A duplicate push gets
-- `result` back verbatim; the same event_id with another payload is refused (EVENT_ID_REUSED).
CREATE TABLE event_inbox (
  business_id      uuid NOT NULL,
  event_id         uuid NOT NULL,
  register_id      uuid NOT NULL,
  local_sequence   bigint NOT NULL CHECK (local_sequence > 0),
  event_type       text NOT NULL,
  schema_version   int NOT NULL,
  occurred_at      timestamptz NOT NULL,
  received_at      timestamptz NOT NULL DEFAULT now(),
  payload          jsonb NOT NULL,
  payload_sha256   bytea NOT NULL,
  result_status    text NOT NULL CHECK (result_status IN ('applied','conflict','rejected')),
  result           jsonb NOT NULL,
  PRIMARY KEY (business_id, event_id),
  UNIQUE (business_id, register_id, local_sequence),
  FOREIGN KEY (business_id, register_id) REFERENCES registers(business_id, register_id)
);

-- One row per register; pushes lock it (FOR UPDATE), so a register's events apply one at a
-- time even when two pushes race.
CREATE TABLE register_sync_state (
  business_id        uuid NOT NULL,
  register_id        uuid NOT NULL,
  ack_sequence       bigint NOT NULL DEFAULT 0 CHECK (ack_sequence >= 0),
  last_push_at       timestamptz,
  last_pull_cursor   bigint,
  last_clock_skew_ms int,
  PRIMARY KEY (business_id, register_id),
  FOREIGN KEY (business_id, register_id) REFERENCES registers(business_id, register_id)
);

-- The change feed other registers pull (SYN-008). `aggregate_id` is text: a setting's id is
-- its key (spec C0003 said uuid).
CREATE TABLE change_log (
  server_seq        bigserial PRIMARY KEY,
  business_id       uuid NOT NULL,
  aggregate_type    text NOT NULL,
  aggregate_id      text NOT NULL,
  version           bigint,
  change_kind       text NOT NULL CHECK (change_kind IN ('upsert','archive','fact')),
  origin_register   uuid,
  origin_event_id   uuid,
  snapshot          jsonb NOT NULL,
  created_at        timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX ix_change_log_business_seq ON change_log(business_id, server_seq);

-- The inbox and the feed are append-only for the application role.
GRANT SELECT, INSERT ON event_inbox, change_log TO pos_app;
GRANT SELECT, INSERT, UPDATE ON register_sync_state TO pos_app;
GRANT USAGE ON SEQUENCE change_log_server_seq_seq TO pos_app;

DO $$
DECLARE t text;
BEGIN
  FOREACH t IN ARRAY ARRAY['event_inbox', 'register_sync_state', 'change_log'] LOOP
    EXECUTE format('ALTER TABLE %I ENABLE ROW LEVEL SECURITY', t);
    EXECUTE format('ALTER TABLE %I FORCE ROW LEVEL SECURITY', t);
    EXECUTE format(
      'CREATE POLICY tenant_isolation ON %I
         USING      (business_id = nullif(current_setting(''app.business_id'', true), '''')::uuid)
         WITH CHECK (business_id = nullif(current_setting(''app.business_id'', true), '''')::uuid)', t);
  END LOOP;
END $$;
