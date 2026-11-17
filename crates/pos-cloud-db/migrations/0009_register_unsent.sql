-- E14-T05: what each register last said it still holds. A register reports, with every
-- pull, how many of its events the cloud has not accepted yet (pending, or refused). The
-- consolidated report uses it to say whether its figures are complete (PRD §15).

ALTER TABLE register_sync_state
  ADD COLUMN unsent_reported    bigint CHECK (unsent_reported >= 0),
  ADD COLUMN unsent_reported_at timestamptz;
