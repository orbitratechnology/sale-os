-- 0016: restoring a linked register (E19-T03). After a restore, `replay_after` holds the last
-- outbox sequence the restored database has: this register's own events after it are brought
-- back from the cloud before anything new is written. NULL the rest of the time.

ALTER TABLE sync_state ADD COLUMN replay_after INTEGER;
