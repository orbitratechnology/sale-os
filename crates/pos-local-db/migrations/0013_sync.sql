-- 0013: sync conflicts and inbound change tracking (Stage 3; SYN-010, AT-07). Spec:
-- docs/backlog/04-local-database.md. Append-only once merged to main.
CREATE TABLE sync_conflicts (                          -- SYN-010, AT-07
  conflict_id          TEXT PRIMARY KEY,
  aggregate_type       TEXT NOT NULL,
  aggregate_id         TEXT NOT NULL,
  outbox_event_id      TEXT NOT NULL REFERENCES outbox_events(event_id),
  local_payload_json   TEXT NOT NULL CHECK (json_valid(local_payload_json)),
  server_payload_json  TEXT NOT NULL CHECK (json_valid(server_payload_json)),
  local_base_version   INTEGER NOT NULL,
  server_version       INTEGER NOT NULL,
  status               TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open','resolved','superseded')),
  resolution_json      TEXT CHECK (resolution_json IS NULL OR json_valid(resolution_json)),
  resolved_by          TEXT,
  resolved_at          INTEGER,
  created_at           INTEGER NOT NULL
) STRICT;
CREATE INDEX ix_conflicts_open ON sync_conflicts(status, aggregate_type);

-- Inbound changes applied (diagnostics + idempotent re-apply guard).
CREATE TABLE inbound_changes (
  server_seq      INTEGER PRIMARY KEY,
  aggregate_type  TEXT NOT NULL,
  aggregate_id    TEXT NOT NULL,
  version         INTEGER,
  applied_at      INTEGER NOT NULL,
  outcome         TEXT NOT NULL CHECK (outcome IN ('applied','skipped_older','skipped_own','conflict_pending'))
) STRICT;
