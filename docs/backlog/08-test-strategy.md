# 08 — Test Strategy and Acceptance Mapping

## 1. Test layers

| Layer | Tooling | Location | Runs | What it proves |
|---|---|---|---|---|
| Domain unit and property | `cargo test`, `proptest` | `crates/pos-domain/src/**` | Every PR | Money, quantity, pricing, settlement, returns, inventory rules, permissions (PRD §16.1) |
| Golden files | JSON/bytes goldens + `insta` | `tests/fixtures/{pricing,receipts,escpos}` | Every PR | Pricing scenarios, ReceiptDocument, ESC/POS bytes, PDF text |
| Store integration | `sqlx` on temp-file SQLite | `crates/pos-local-db/tests` | Every PR | Migrations (fresh and upgrade), constraints, triggers, `apply` atomicity with `FailAt(n)`, invariants (PRD §16.2) |
| Application | Services wired with the real store and fake clock/IDs | `crates/pos-application/tests` | Every PR | Use cases end to end below IPC, the permission matrix (AT-11) |
| IPC contract | Generated bindings + command registry tests | `apps/desktop-tauri/tests` | Every PR | Every command is in the matrix, errors map to codes, bindings are current |
| Cloud integration | testcontainers Postgres + `cloud-api` in-process | `tests/integration` | Every PR (smoke), nightly (full) | Ingestion idempotency, RLS and tenancy (AT-12), appliers, pull |
| Desktop ↔ cloud | Desktop core (headless) + real `cloud-api` + `FaultyTransport` | `tests/integration/sync_*` | Nightly; smoke on PR | AT-06 and AT-07, replay and convergence under faults |
| E2E UI | Playwright via `tauri-driver` (WebDriver) on Windows, virtual devices | `tests/e2e` | Every PR (core flows), nightly (all) | The cashier flows, keyboard-only, a11y (axe), visual receipts |
| Crash and durability | `xtask crash-harness`, VM power-off campaign | `xtask/`, `docs/perf` | Nightly (100 kills), Stage 4 campaign | No partial commits, no lost acknowledged sales |
| Performance | `criterion` + e2e timing marks | `benches/`, `tests/e2e/perf` | Weekly, plus Stage 4 on reference hardware | PRD §14 targets |
| Security | `cargo audit`/`deny`, `pnpm audit`, `gitleaks`, PAN canary test, external pen test | CI + Stage 4 | Every PR / Stage 4 | PRD §13 |

## 2. Fixtures and simulators

- **Catalogue profiles** (`xtask seed`):
  - `demo20` (AT-01)
  - `shop5k` (UI perf)
  - `bench50k` (50k variants, 100k barcodes)
  - `history500k` (500k sale lines over 365 days, for reports)
- **CSV fixtures:**
  - valid
  - duplicates in file and against existing data
  - missing prices
  - invalid tax categories
  - malformed rows
  - Windows-1252 encoding
  - semicolon-delimited
  - 50k rows
- **Pricing goldens:** at least 40 scenarios. Product/finance sign them off for the D-02 country.
- **Fixture day** (E15-T03): 12 sales, 1 receipt, 1 adjustment, 2 returns, a cash session.
  Every expected figure is calculated by hand in `tests/fixtures/day1/expected.json`.
- **Virtual devices:**
  - `VirtualScanner`, `VirtualPrinter` and `MockPaymentProvider`, with their scenario APIs
    ([06 §10](06-rust-interfaces.md#10-device-ports--pos-device-ports-e07-e13-e10))
  - `FaultyTransport` (E17-T10)
  - `FixedClock`/`SteppingClock`, for timezone, DST and clock-drift cases (PRD §16.3)
- **Never** real card data. The PAN canary uses the public test number `4111 1111 1111 1111`,
  and only to prove it is rejected and never stored.

## 3. MVP acceptance tests (PRD §16.4) — automation mapping

Each acceptance test is a named test in CI. `E19-T12` makes them a required, green gate on the
reference machine.

| AT | PRD statement (abridged) | Automated test | Layer | First green in |
|---|---|---|---|---|
| AT-01 | New business: profile, ≥ 20 products, test cash sale, no cloud | `e2e::at01_offline_first_business` (the network adapter is disabled in the VM, or the transport is forced offline) | E2E | Stage 2 (E09-T08) |
| AT-02 | Find a product by barcode, SKU and name | `e2e::at02_find_by_barcode_sku_name` + `app::catalog_lookup_*` | E2E + app | Stage 1 (E06-T04, E09-T04) |
| AT-03 | Repeated scans increment correctly; no lost rapid scans | `e2e::at03_burst_scans` (virtual scanner, 5 ms key interval, 50 ms gaps) + `app::scan_dedupe_by_event_id_only` | E2E + app | Stage 1 (E07-T02) |
| AT-04 | A completed sale and all dependent records commit atomically | `store::at04_complete_sale_atomic_failat_all_n` + `e2e::at04_failure_shows_error_cart_intact` | Store + E2E | Stage 2 (E09-T08) |
| AT-05 | A restart loses no committed sales or pending events | `crash::at05_kill_after_commit` + nightly crash harness | Crash | Stage 2 (E09-T08, E02-T04) |
| AT-06 | An offline sale syncs exactly once despite retries | `sync::at06_offline_replay_exactly_once` (all `FaultyTransport` scenarios) | Desktop ↔ cloud | Stage 3 (E17-T10) |
| AT-07 | A conflicting product edit is surfaced, not overwritten | `sync::at07_concurrent_price_edit_conflict` + `e2e::at07_conflict_ui` | Desktop ↔ cloud + E2E | Stage 3 (E18-T02) |
| AT-08 | Preview a receipt and export it as PDF without a printer | `e2e::at08_receipt_preview_pdf_no_printer` + `receipts::pdf_golden_*` | E2E + golden | Stage 2 (E13-T04) |
| AT-09 | ESC/POS output captured via the virtual printer | `receipts::at09_escpos_capture_matches_golden` | Golden | Stage 2 (E13-T06) |
| AT-10 | A return creates a linked return/refund and the correct stock movement | `app::at10_return_dispositions` (restock / damaged / none) | App | Stage 2 (E12-T02) |
| AT-11 | A cashier cannot do manager-only operations by invoking IPC directly | `ipc::at11_permission_matrix_direct_invoke` (every command × every role) | IPC contract | Stage 1 (E05-T03) |
| AT-12 | A business cannot read another business's cloud data | `cloud::at12_cross_tenant_all_routes` + `cloud::at12_rls_sql_level` | Cloud integration | Stage 3 (E16-T05) |
| AT-13 | The UI distinguishes pending vs synchronized, and external confirmation vs processor approval | `e2e::at13_sync_state_labels` + `e2e::at13_external_tender_label` + `domain::provider_confirmation_requires_payment_port` | E2E + domain | Stage 3 (E17-T09); the label part in Stage 2 (E10-T03) |

## 4. Quality gates per PR

- [ ] fmt, clippy (`-D warnings`), eslint, typecheck
- [ ] Unit, property, golden, store, application, IPC contract and cloud smoke tests
- [ ] Bindings, OpenAPI and event schemas regenerated, with no diff; schema-compat passes
- [ ] `cargo deny`, `gitleaks`
- [ ] E2E core flows on Windows
- [ ] If a migration changed: the upgrade-path test from the last released schema
- [ ] If pricing or rounding changed: the goldens are updated in the same PR, and a CODEOWNERS
  approval from finance or product is present

## 5. Nightly

- The full e2e suite, including simulator matrices (scanner, printer, payment)
- The full `FaultyTransport` sync matrix
- The crash harness (100 kills)
- Performance smoke at `bench50k`, with regression alerts above 20%
