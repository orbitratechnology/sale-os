-- 0008: the change feed names each row's event type and schema version (E17-T07), so a
-- register can parse a pulled snapshot (a cash session's opening and closing share an
-- aggregate type), and keys pulls by business and position. Rows written before this
-- migration have none and are skipped by readers. Append-only once merged to main.
ALTER TABLE change_log ADD COLUMN event_type text;
ALTER TABLE change_log ADD COLUMN schema_version int;
