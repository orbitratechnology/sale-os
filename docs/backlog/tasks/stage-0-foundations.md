# Stage 0 — Architecture and Technical Spikes

**Goal.** Prove the architecture end to end before building features, and remove the riskiest
unknowns: Tauri packaging on Windows, SQLite durability, typed IPC, keyboard-wedge capture,
ESC/POS capture, and domain-crate reuse in Axum.

**Exit gate (PRD §17).** One desktop shell can create a local database, store a product,
simulate a barcode scan, and commit a demo sale atomically. This is shown by
`E00-T06` running in CI on Windows.

**Rough size.** ≈ 45–60 engineer-days.

---

## E00 Engineering foundation

### E00-T01 · Monorepo scaffold
- **Size** S · **Deps** — · **PRD** §8.4
- **Scope:**
  - Cargo workspace with every crate from [02 §1](../02-architecture-and-modules.md#1-repository-layout) as an empty library with a doc comment.
  - `rust-toolchain.toml` pinned to stable.
  - pnpm workspace for `apps/desktop-ui`.
  - `.editorconfig`, `rustfmt.toml`, `clippy.toml`, ESLint and Prettier, TS `strict`.
  - `xtask` crate stub.
- **Acceptance:**
  - [ ] `cargo build --workspace` and `pnpm -r build` succeed on a clean checkout (Windows and Linux).
  - [ ] The crate dependency graph matches [02 §3](../02-architecture-and-modules.md#3-dependency-rules-enforced-in-ci-by-cargo-deny-bans-and-an-xtask-check-deps-graph-test), and `cargo xtask check-deps` fails on a forbidden edge (a test adds one and asserts the failure).
  - [ ] `README.md` explains the prerequisites and the one-command dev start.

### E00-T02 · CI pipeline
- **Size** M · **Deps** E00-T01 · **PRD** §20
- **Scope:** GitHub Actions running on `windows-latest` and `ubuntu-latest`:
  - `cargo fmt --check`
  - `clippy -D warnings`
  - `cargo test`
  - `cargo deny check` (licences, advisories, bans)
  - `pnpm lint typecheck test`
  - a Tauri debug build on Windows, uploaded as an artefact
  - caching
  - `xtask gen-bindings --check` and `gen-schemas --check` (no-ops until those generators exist)
- **Acceptance:**
  - [ ] A PR with a clippy warning, a fmt diff, or a failing test is blocked.
  - [ ] A dependency with a disallowed licence (GPL-3.0 in the list) fails `cargo deny`.
  - [ ] Pipeline wall time is under 15 min with a warm cache.

### E00-T03 · ADR records
- **Size** S · **Deps** — · **PRD** §19
- **Scope:** Create `docs/adr/0001…0020` from [03 Part 1](../03-decisions.md) using a template (context, decision, consequences, status).
- **Acceptance:**
  - [ ] Each ADR is reviewed and accepted by the tech lead plus one other engineer.
  - [ ] There is a PR template checkbox: "Does this change an ADR?"

### E00-T04 · Dev tooling: seed and reset
- **Size** S · **Deps** E02-T02, E06-T01 (finished in Stage 1) · **PRD** §16
- **Scope:**
  - `cargo xtask db-reset`, which deletes the dev database and runs migrations.
  - `cargo xtask seed --profile {demo20|shop5k|bench50k}`, which generates deterministic catalogues (fixed seed) through the **application services**, not raw SQL.
- **Acceptance:**
  - [ ] `seed --profile demo20` creates 20 products with barcodes, two tax rates and one variant product (size/colour).
  - [ ] Two runs with the same seed produce identical IDs and data.
  - [ ] `bench50k` creates 50k variants and 100k barcodes in under 3 min.

### E00-T05 · Contracts folder and generators
- **Size** S · **Deps** E00-T01 · **PRD** §8.1, §10.3
- **Scope:**
  - `contracts/openapi/pos-cloud.v1.yaml` skeleton with `/health` and `/version-policy`, plus Spectral lint.
  - `contracts/events/` with a `schemars` generator wired into `xtask gen-schemas`.
- **Acceptance:**
  - [ ] Spectral lint runs in CI.
  - [ ] Changing a payload type without regenerating fails CI.

### E00-T06 · Stage 0 vertical slice (exit gate)
- **Size** M · **Deps** E01-T02, E02-T03, E03-T02, E07-T01, E13-T01, E16-T01
- **Scope:** A throwaway but real flow:
  - Launch the app, which creates the database.
  - "Add demo product" (command).
  - The virtual scanner scans its barcode, and a cart line appears.
  - "Complete cash sale" runs one `WriteSet` (sale, line, tender, movement, audit, outbox).
  - The virtual printer captures ESC/POS bytes.
  - Spike tables are allowed (see [04](../04-local-database.md)). The UI can be unstyled.
- **Acceptance:**
  - [ ] An automated test drives the flow through the IPC layer and asserts:
    - 1 sale, 1 tender, 1 movement, 1 audit row and 1 outbox row exist
    - `outbox.local_sequence = 1`
    - a capture file exists
  - [ ] Fault injection: making the movement insert fail (test hook) leaves **zero** new rows in every table (AT-04 precursor).
  - [ ] The demo runs from the Windows CI artefact on a clean Windows 11 VM.

## E01 Desktop shell and IPC

### E01-T01 · Tauri 2 scaffold and Windows packaging spike
- **Size** M · **Deps** E00-T01 · **PRD** §8.1, §17 Stage 0
- **Scope:**
  - Tauri 2 with React, TypeScript and Vite.
  - Produce an NSIS installer and an MSI.
  - WebView2 bootstrapper mode: decide between `embedBootstrapper` and `offlineInstaller` for offline shops, and record it in the ADR.
  - Per-user install into `%LOCALAPPDATA%`.
  - Measure cold start.
- **Acceptance:**
  - [ ] The installer installs and uninstalls cleanly on Windows 11 23H2 without admin rights (per-user).
  - [ ] Cold start to first paint is measured and recorded in `docs/perf/baseline.md`. Target: under 5 s (NFR-launch).
  - [ ] The app runs on a machine **without internet** during install, if the offline WebView2 option is chosen.

### E01-T02 · Typed IPC with generated bindings
- **Size** M · **Deps** E01-T01 · **PRD** §8.2, ADR-012
- **Scope:**
  - `tauri-specta` setup.
  - The `IpcError` type and an `AppError → IpcError` mapping.
  - Money and quantity string serialization helpers on both sides (`lib/money.ts` formats a minor-unit string using the currency exponent, with no float math).
  - An event-emit helper.
- **Acceptance:**
  - [ ] `bindings.ts` is generated, and CI fails on drift.
  - [ ] A TS unit test proves `formatMoney("1999", {exponent:2})` gives `"19.99"`, and that `formatMoney("-5", …)` and 3-decimal currencies are correct, with no `Number` conversion of minor units above 2^53. A lint rule bans `parseFloat` and `Number(` in `features/**` money code.
  - [ ] An unknown error variant maps to `INTERNAL` and is logged with a correlation ID.

### E01-T03 · Capability lockdown and CSP
- **Size** S · **Deps** E01-T01 · **PRD** §13, ADR-012
- **Scope:**
  - `capabilities/main.json` allowing only app commands, the event basics and `dialog:save`/`dialog:open`.
  - A strict CSP: no remote origins, no `unsafe-eval`.
  - Remote URLs disabled.
- **Acceptance:**
  - [ ] An e2e test tries to call `fs`, `shell` and `http` plugin APIs from the WebView, and each call is rejected.
  - [ ] Loading a remote script is blocked by CSP (asserted through a console-error check).

### E01-T04 · AppServices wiring, logging and redaction
- **Size** S · **Deps** E01-T02 · **PRD** §13, §14 (Observability)
- **Scope:**
  - An `AppServices` container that builds ports and services.
  - `tracing` with a JSON file appender (rolling, 7 days) in the app data `logs/` folder.
  - A redaction layer for fields named `pin`, `password`, `token`, `refresh_token` and `authorization`, and for any `SecretString`.
- **Acceptance:**
  - [ ] A unit test logs a struct containing a PIN and a token, and the output contains `"[REDACTED]"` only.
  - [ ] Each log line has `ts`, `level`, `target`, `correlation_id` and `register_id` (when known).

## E02 Local persistence

### E02-T01 · SQLite connection layer
- **Size** S · **Deps** E00-T01 · **PRD** §9.1, ADR-007
- **Scope:**
  - `sqlx` with SQLite: one writer connection behind an actor (mpsc), plus a reader pool of 4.
  - The pragmas from ADR-007 are applied on every connection.
  - The database path resolves to the app data directory, and UNC/network paths are refused.
- **Acceptance:**
  - [ ] A test asserts `journal_mode=wal`, `synchronous=2 (FULL)`, `foreign_keys=1` and `busy_timeout=5000` on both reader and writer connections.
  - [ ] Starting with a `\\server\share\pos.db` path fails with `STORE_IO`, and the message explains why.
  - [ ] 1,000 concurrent read queries during a write loop produce no `SQLITE_BUSY`.

### E02-T02 · Migration runner and 0001 skeleton
- **Size** M · **Deps** E02-T01 · **PRD** §9.1
- **Scope:**
  - Embedded `sqlx::migrate!`.
  - **A pre-migration backup** (`VACUUM INTO backups/pre-migrate-<from>-<to>.db`).
  - Refuse to open a database whose schema version is newer than the app ("downgrade guard").
  - The `0001_core.sql` tables `register_counters`, `audit_events`, `outbox_events` and `sync_state`, with the remaining 0001 tables added in E04-T01. This is allowed **only before 0001 merges to main**; after that, new migrations.
- **Acceptance:**
  - [ ] A fresh database migrates to head.
  - [ ] A database at version N migrates to head (an upgrade-path test with fixtures per released version, starting at the first pilot build).
  - [ ] A failed migration leaves the original database untouched and the backup present.
  - [ ] Opening a newer-schema database shows an "update the app" error and does not modify the file.

### E02-T03 · `LocalStore::apply` and WriteSet core
- **Size** M · **Deps** E02-T02, E03-T01 · **PRD** §11.2, ADR-008
- **Scope:**
  - The `WriteSet` builder with ADR-008 validation.
  - `apply` runs `BEGIN IMMEDIATE`, checks preconditions, allocates counters, inserts rows, writes the outbox envelope (sequence assigned here), commits, then emits change notifications.
  - Debug-build invariant checks.
- **Acceptance:**
  - [ ] `build()` fails without an audit entry, and fails when a syncable record has no outbox event (unit tests).
  - [ ] Over 10,000 applies from 8 concurrent tasks, `local_sequence` has no gaps or duplicates, and `register_counters.outbox_seq = MAX(local_sequence)` (property test).
  - [ ] A failure injected at each statement index (test hook `FailAt(n)`) rolls back everything. Row counts before and after are identical for every `n`.
  - [ ] The p95 of a 20-line demo sale `apply` is under 50 ms on CI hardware (this is the headroom budget for NFR-commit < 500 ms).

### E02-T04 · Crash-consistency harness (spike)
- **Size** M · **Deps** E02-T03 · **PRD** §14 (Crash recovery), §14.1, ADR-007
- **Scope:**
  - `xtask crash-harness` spawns a child process that commits sales in a loop. The parent kills it with `TerminateProcess`/`SIGKILL` at random times, 500 times. After each kill it reopens the database and checks invariants.
  - The Windows run is required.
- **Acceptance:**
  - [ ] Across 500 kills, there is no partially committed sale: every sale has its lines, tenders, movements, audit row and outbox row, or none of them.
  - [ ] `quick_check` passes every time.
  - [ ] Every sale the child reported as committed (written to a side file *after* commit returned) exists after reopening.
  - [ ] The results are written to `docs/perf/crash-harness.md`. The harness stays in CI as a nightly job with 100 kills.

## E03 Domain primitives

### E03-T01 · IDs, Clock and IdGenerator
- **Size** S · **Deps** E00-T01 · **ADR** 005, 006
- **Acceptance:**
  - [ ] `id_type!` newtypes round-trip through serde, `sqlx` TEXT and `Display`/`FromStr`.
  - [ ] `FixedClock`, `SteppingClock` and `SeqIdGenerator` fakes are available in `pos-testkit`.
  - [ ] `business_date` is correct across DST transitions and at 23:59:59.999 and 00:00 local time, for `Europe/London`, `America/New_York`, `Asia/Kolkata` (+5:30) and `Pacific/Chatham` (+12:45).

### E03-T02 · Money, Currency, Quantity, RatePpm
- **Size** M · **Deps** E03-T01 · **PRD** §9.3, ADR-001/002/003
- **Acceptance:**
  - [ ] Checked arithmetic never panics (proptest over the full `i64` range).
  - [ ] `Currency::parse`/`format` handle exponents 0, 2 and 3, negative values, and locale separators for en-US and de-DE.
  - [ ] `Quantity::validate_for_unit` rejects 1.5 for `ea` and accepts 1.5 for `kg`.
  - [ ] `RatePpm::new(1_000_001)` returns an error.
  - [ ] Binary floats appear nowhere in the public API. A `clippy::float_arithmetic` deny is set for `pos-domain`.

### E03-T03 · Rounding and allocation
- **Size** S · **Deps** E03-T02 · **ADR** 004
- **Acceptance:**
  - [ ] `round_to_minor(2.5) = 3`, `round_to_minor(-2.5) = -3` (half away from zero), and `HalfEven(2.5) = 2`.
  - [ ] `allocate(100, [1,1,1]) = [34,33,33]` (deterministic: ties go to the lowest index).
  - [ ] proptest: `Σ allocate(t, w) = t` for any non-negative weights.
  - [ ] All-zero weights split equally.

### E03-T04 · Error model and catalogue
- **Size** S · **Deps** E01-T02
- **Acceptance:**
  - [ ] Every domain error variant maps to exactly one code from [07 §2](../07-contracts.md#2-error-model). An exhaustive match test fails when a variant is added without a mapping.
  - [ ] `docs/errors.md` is generated from the catalogue.

## E07 Barcode (spike)

### E07-T01 · Keyboard-wedge capture spike
- **Size** M · **Deps** E01-T02 · **PRD** BAR-002, BAR-007, §12.3
- **Scope:**
  - A `ScanCapture` React hook that tells scanner bursts from human typing (inter-key gap of 35 ms or less, configurable; minimum 4 characters; terminated by the configured suffix of Enter, Tab or none + timeout). It produces a `scanEventId` (UUID) per burst and calls `scan_submit`.
  - It works when focus is in the scan field **and** in "scanner-ready" mode, where focus is elsewhere on the checkout screen but not in another text input.
- **Acceptance:**
  - [ ] Playwright-driven synthetic key bursts at 5 ms per key, 20 scans with 50 ms gaps: all 20 are delivered, with no merging and no drops (BAR-007, AT-03).
  - [ ] A human typing "abc" at 150 ms per key in a search field is **not** treated as a scan.
  - [ ] Suffix modes Enter, Tab and none (timeout of 80 ms) each pass the suite.
  - [ ] Findings, including WebView2 key-event latency, are recorded in an ADR addendum.

## E13 Receipts (spike)

### E13-T01 · ESC/POS encoder and virtual printer capture (spike)
- **Size** S · **Deps** E00-T01 · **PRD** RCP-005, §12.4
- **Acceptance:**
  - [ ] Encodes init, text, bold, align, feed and partial cut for a 3-line receipt. The bytes match a hand-verified golden file.
  - [ ] The virtual printer writes `.bin` and a human-readable `.txt` decode to `target/virtual-printer/`.

## E16 Cloud (spike)

### E16-T01 · Cloud API skeleton reusing the domain
- **Size** S · **Deps** E03-T02 · **PRD** §17 Stage 0 ("validate Rust domain crate reuse in Axum")
- **Scope:**
  - An Axum app with `/v1/health`, config through env vars, `sqlx` Postgres, a migration runner, and a Dockerfile.
  - A `/v1/_debug/price` endpoint (dev only) that calls `pos-domain` pricing, to prove reuse.
- **Acceptance:**
  - [ ] An integration test with testcontainers Postgres starts the API and gets 200 from `/v1/health`.
  - [ ] `cloud-api` depends on `pos-domain` with no feature flags or forks. `cargo tree` shows no `tauri` or `sqlite` dependency.
