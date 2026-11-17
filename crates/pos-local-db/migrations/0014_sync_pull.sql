-- 0014: pulling from the cloud (E17-T07). Append-only once merged to main.

-- Resuming an interrupted bootstrap: the next page's token while one is under way; cleared,
-- and `last_pull_cursor` set, after the last page.
ALTER TABLE sync_state ADD COLUMN bootstrap_page TEXT;

-- The inbound rule asks, per pulled change, whether the aggregate has an unacknowledged
-- local edit.
CREATE INDEX ix_outbox_aggregate ON outbox_events(aggregate_type, aggregate_id, local_sequence);

-- Each variant's row in the search index. `product_search.variant_id` is UNINDEXED, so
-- deleting a product's rows by it scanned the whole index on every product write; by rowid
-- it is a lookup.
CREATE TABLE search_index_rows (
  variant_id    TEXT PRIMARY KEY,
  search_rowid  INTEGER NOT NULL UNIQUE
) STRICT;
INSERT INTO search_index_rows (variant_id, search_rowid) SELECT variant_id, rowid FROM product_search;

-- Linking (E17-T08): a link under way (JSON `LinkProgress`), so it can resume after a lost
-- connection; and a joining register's downloaded bootstrap, applied only once complete.
ALTER TABLE sync_state ADD COLUMN link_progress TEXT CHECK (link_progress IS NULL OR json_valid(link_progress));
CREATE TABLE bootstrap_staging (
  position     INTEGER PRIMARY KEY,
  entity_json  TEXT NOT NULL CHECK (json_valid(entity_json))
) STRICT;
