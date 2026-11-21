# tests/fixtures

See docs/backlog/08-test-strategy.md for what belongs here.

- `pricing/`: the ADR-004 golden scenarios (E08-T03). Edit `pricing/generate.py`, never the
  JSON, then run `python3 tests/fixtures/pricing/generate.py`. Read by
  `crates/pos-domain/tests/pricing_goldens.rs` and `apps/desktop-ui/src/lib/pricingGoldens.test.ts`.
- `sync/end_of_day.json`: the end-of-day event stream (`UPDATE_SYNC_FIXTURE=1`).
