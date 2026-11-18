-- E19-T03: a restored register gets its own events back from the inbox (`GET
-- /v1/sync/replay`), which needs each event's aggregate and base version as it pushed them.
-- Rows written before this migration have NULLs; nothing had been restored from them.

ALTER TABLE event_inbox
  ADD COLUMN aggregate_type text,
  ADD COLUMN aggregate_id   text,
  ADD COLUMN base_version   bigint;
