-- 0015: consolidated reports kept on this register (E14-T05). The last report fetched from
-- the cloud for each range and grouping, shown marked stale while the cloud is out of
-- reach (PRD §11.6). Local only; a join empties it with the rest of the business data.

CREATE TABLE cloud_report_cache (
  cache_key    TEXT PRIMARY KEY,
  report_json  TEXT NOT NULL,
  fetched_at   INTEGER NOT NULL
) STRICT;
