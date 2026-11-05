# Stage 3 — Offline Cloud Synchronization

**Goal.** The cloud account, device registration, transactional-outbox delivery, idempotent
ingestion, master-data pull, conflict handling, cloud backup, and consolidated reporting.

**Exit gate (PRD §17).** Sales made while disconnected replay correctly after reconnection,
with no duplicate business effects. This is proven by the `E17-T10` fault-injection suite,
which uses a real Postgres and a real `cloud-api` and covers AT-06, AT-07 and AT-12.

**Rough size.** ≈ 70–85 engineer-days. About 25 of those (E16-T02..T05, E17-T01) can be pulled into Stage 2 by the cloud engineer.

**Decisions needed at the start:** D-03, D-06, D-09, D-10.

---

## E16 Cloud platform

### E16-T02 · Cloud schema: identity, tenancy and RLS foundation
- **Size** M · **Deps** E16-T01 · **PRD** §10.1, §10.2, ADR-016
- **Scope:**
  - Migrations C0001, C0002 and C0007 (the policy pattern).
  - The roles `pos_owner` (migrations) and `pos_app` (runtime, no `BYPASSRLS`).
  - `TenantTx::begin(pool, principal)`.
- **Acceptance:**
  - [ ] Every tenant table has RLS `ENABLE` and `FORCE` set. A catalogue query test fails if any table holding `business_id` lacks a policy.
  - [ ] As `pos_app`, a query without `app.business_id` set returns 0 rows (fails closed).
  - [ ] Repositories accept only `TenantTx`. This is a compile-time check, documented with a `compile_fail` doctest.

### E16-T03 · Owner account authentication
- **Size** M · **Deps** E16-T02 · **PRD** §7.1 step 2, D-09, ADR-013
- **Scope:**
  - `/auth/signup` and `/auth/login`.
  - Argon2id.
  - Rate limiting per IP and per email: 5 per minute, then exponential.
  - User JWT (15 min).
  - An `IdentityProvider` trait so OIDC can replace this later.
  - No email verification in the MVP (it is behind a flag; IAM-006 is P1).
- **Acceptance:**
  - [ ] Wrong-password and unknown-email responses are identical in body and timing (within ±10 ms on average over 100 runs).
  - [ ] Passwords must be at least 10 characters and are checked against a top-10k breached list (an embedded file).
  - [ ] Logs never contain passwords or tokens (redaction test).

### E16-T04 · Device registration, token rotation and revocation
- **Size** M · **Deps** E16-T03 · **PRD** SYN-001, §13, ADR-013
- **Scope:**
  - `POST /businesses` (idempotent, with a client ID), `/devices/register`, `/auth/device/refresh` (rotation with reuse detection), `/devices/{id}/revoke` and `GET /devices`.
  - Client side: a `CredentialStore` using the OS keychain, and a token refresher in the transport.
- **Acceptance:**
  - [ ] Reusing a rotated refresh token revokes the device. The next push gets `401 SYNC_DEVICE_REVOKED`, and the desktop shows "This register was disconnected by the owner" and continues **local-only** (local sales still work).
  - [ ] Registering a second device for the same register revokes the first, after the UI confirms.
  - [ ] The refresh token is in Windows Credential Manager, not in SQLite. The test greps the database file and the log files for the token.
  - [ ] Access JWT claims: `business_id`, `register_id`, `device_id`, `exp`. They are validated on every request, and a revoked device is rejected within one access-token lifetime (≤ 15 min). Pushes do an immediate revocation check by looking up the device status.

### E16-T05 · Tenant isolation middleware and test suite
- **Size** M · **Deps** E16-T04 · **PRD** §10.2, AT-12
- **Acceptance:**
  - [ ] **AT-12:** a generated test hits every route with the principal of business A and IDs from business B. The results are 404 or empty, never B's data. Body-supplied `businessId` and `registerId` that differ from the token give 403.
  - [ ] A SQL-level test: a direct `SELECT` as `pos_app` with `app.business_id = A` on each table returns no B rows.
  - [ ] Added to CI as a required check.

### E16-T06 · OpenAPI finalization and contract tests
- **Size** S · **Deps** E16-T04, E17-T01
- **Acceptance:**
  - [ ] `pos-cloud.v1.yaml` covers every route in [07 §3](../07-contracts.md#3-cloud-rest-api-v1) with schemas and problem+json errors.
  - [ ] A contract test validates real responses against the spec for every route (`schemathesis` in CI against a test instance).
  - [ ] The desktop `HttpSyncTransport` DTOs deserialize the spec examples.

### E16-T07 · Deployment, environments and database backups
- **Size** M · **Deps** E16-T01, D-06
- **Scope:**
  - Container image.
  - Infrastructure as code for staging and production: the API service, the worker service, managed Postgres with point-in-time recovery, and object storage.
  - Migrations run as a pre-deploy job.
  - Secrets come from the platform secret store.
  - TLS and HSTS.
  - Health checks.
- **Acceptance:**
  - [ ] One-command deploy to staging from CI on merge to `main`. Production deploy is manual-approval.
  - [ ] Daily logical backup to object storage (encrypted, 35-day retention). A **restore drill** is scripted and run once, with a documented recovery time (feeds E16-T09).
  - [ ] No secrets in the image or the repo (`gitleaks` in CI).

### E16-T08 · Cloud observability
- **Size** S · **Deps** E16-T07 · **PRD** §13, §14
- **Acceptance:**
  - [ ] Structured logs with request ID, business ID (hashed in logs) and route. Payloads are never logged.
  - [ ] Metrics: request rate, latency and errors per route; events ingested per status; ingestion lag; pull lag. An alert on a rejected-event spike.
  - [ ] Error reporting with PII scrubbing.

### E16-T09 · Cloud backup and restore verification
- **Size** S · **Deps** E16-T07 · **PRD** §10.1, §13, §20
- **Acceptance:**
  - [ ] A weekly automated job restores the latest backup into a scratch database, runs the row-count and checksum queries against production, and reports the result.
  - [ ] A runbook exists for restoring one tenant's data.

## E17 Sync engine

### E17-T01 · Event envelope, payload types and schema governance
- **Size** M · **Deps** E00-T05 · **PRD** §11.3, SYN-005
- **Scope:**
  - The `pos-sync::events` types for every event in [07 §4](../07-contracts.md#4-sync-event-catalogue-pos-syncevents).
  - `schemars` generation.
  - An `xtask schema-compat` that diffs against the last released schemas and fails on breaking changes made without a version bump.
  - Upcaster registry.
  - Migration `0011_sync.sql`.
- **Acceptance:**
  - [ ] Every outbox event written in Stages 1–2 validates against its generated JSON Schema (a test replays the E15-T03 scenario outbox).
  - [ ] Removing a field without bumping `schemaVersion` fails `schema-compat`.
  - [ ] The envelope includes every PRD §11.3 field.

### E17-T02 · Outbox coverage audit
- **Size** S · **Deps** E17-T01 · **PRD** SYN-003, ADR-009
- **Acceptance:**
  - [ ] A test enumerates every syncable table. For a scripted run touching all of them, every inserted or updated syncable row has a matching outbox event in the same transaction (via `outbox_event_id` on the audit row and the aggregate ID).
  - [ ] Local-only tables (carts, print jobs, throttle, sessions, import) emit nothing.

### E17-T03 · Push worker
- **Size** M · **Deps** E17-T01, E16-T04 · **PRD** SYN-004, SYN-006, SYN-007, ADR-010
- **Scope:**
  - `SyncWorker` push loop: batches of up to 200 events / 2 MB in `local_sequence` order, starting at `last_ack_sequence + 1`.
  - Applies `PushResponse` per event: `applied`/`duplicate` → handled; `conflict` → `sync_conflicts` row; `rejected` → repair queue; `out_of_order` → rewind.
  - Advances `last_ack_sequence` **only** to the server's `ackSequence`.
  - Backoff with full jitter.
  - Triggers: after commit, on connectivity change, every 30 s while there is pending work, and `sync_retry_now`.
  - Records server time and skew.
- **Acceptance:**
  - [ ] The ack never exceeds the server's reported `ackSequence` (property test with a fake transport returning arbitrary valid responses).
  - [ ] A transient error (timeout, 503) causes no state change and a retry with growing delay (fake clock asserts the delays fall within jitter bounds: 2 s, 4 s … capped at 5 min).
  - [ ] 10,000 pending events (a week offline at a busy shop) drain in under 5 min against the staging API.
  - [ ] Pushing never blocks checkout. Checkout commit p95 is unchanged while the worker pushes (bench).
  - [ ] Clock skew > 10 min sets the `clockSkewWarning` (ADR-006).

### E17-T04 · Server ingestion framework
- **Size** M · **Deps** E16-T05, E17-T01 · **PRD** SYN-005, SYN-006, §11.3, ADR-010
- **Scope:**
  - Migration C0003.
  - `POST /v1/sync/events`, which follows the per-register sequential loop in [06 §12](../06-rust-interfaces.md#12-cloud--cloud-api-event-appliers-e17-t04t06).
  - An applier registry.
  - Payload hash check for a reused `event_id`.
  - `GET /v1/sync/ack`.
  - A concurrency guard: a `register_sync_state` row lock, so two concurrent pushes from the same register serialize.
- **Acceptance:**
  - [ ] **Idempotency:** the same batch sent 5 times (and concurrently) gives exactly one application and identical results each time. Business tables are checked for duplicates (AT-06 core).
  - [ ] An event is acknowledged only after commit. Killing the API process after apply but before the response, then retrying, gives a `duplicate` result with the original outcome (SYN-006).
  - [ ] The same `event_id` with a different payload gives `rejected: EVENT_ID_REUSED`, and an alert is raised.
  - [ ] An out-of-order sequence gives `out_of_order` with the expected value.
  - [ ] An unknown event type or a higher schema version gives `rejected: EVENT_VERSION_UNSUPPORTED`, and the event stays visible client-side (§11.3).

### E17-T05 · Fact appliers (sales, returns, inventory, cash sessions, audit)
- **Size** M · **Deps** E17-T04 · **PRD** SYN-009, §11.1, §11.5
- **Scope:**
  - Migration C0005.
  - Appliers insert the facts, update `stock_balances`, update `daily_sales_rollup`, and append to `change_log` (`kind = fact`) for other registers.
  - Validation per [07 §4](../07-contracts.md#fact-events-immutable-never-conflict-validated-may-be-rejected), including `verify_snapshot`.
  - Over-return across registers is flagged.
- **Acceptance:**
  - [ ] A sale is ingested with its totals unchanged. A tampered total gives `rejected: SNAPSHOT_MISMATCH`.
  - [ ] Facts are never updated. `pos_app` has no `UPDATE` grant on fact tables (a SQL test).
  - [ ] Two registers selling the last unit offline both get applied. The cloud balance goes to −1 and is flagged (§11.5).
  - [ ] A return whose original sale has not arrived yet gives `transient ORIGINAL_SALE_PENDING`. When the push includes both in order, it succeeds.

### E17-T06 · Master-data appliers with version checks
- **Size** M · **Deps** E17-T04 · **PRD** SYN-010, §11.4, ADR-011
- **Scope:**
  - Migration C0004.
  - Upsert appliers for each master aggregate: version check, `change_log` append, uniqueness collisions (`BARCODE_TAKEN`, `SKU_TAKEN`) mapped to `conflict` with the owning snapshot.
  - `POST /v1/catalog/products` uses the same applier (origin = cloud).
- **Acceptance:**
  - [ ] An edit with base 3 when the server is at 3 is applied, and the server moves to 4.
  - [ ] An edit with base 3 when the server is at 4 gives `conflict` with the server snapshot. The server record is **not** modified (AT-07 server side).
  - [ ] Two offline registers adding the same barcode to different products: the second gives `conflict: BARCODE_TAKEN`.
  - [ ] Staff PIN hash changes propagate. A deactivated staff user cannot log in on other registers after their next pull.

### E17-T07 · Bootstrap and incremental pull
- **Size** M · **Deps** E17-T06 · **PRD** SYN-002, SYN-008, §11.4
- **Scope:**
  - A spike, then the decision on the change_log visibility approach ([05 C0003](../05-cloud-database.md#c0003-sync_inbox_changelogsql)).
  - `GET /bootstrap` (paged snapshot plus starting cursor) and `GET /changes`.
  - The client pull loop and the inbound applier per the rule in [04 0011](../04-local-database.md#0011_syncsql).
  - Facts from other registers update local balances and the cloud-scope reports.
- **Acceptance:**
  - [ ] Bootstrapping a new register into a business with 50k variants completes in under 3 min on a 20 Mbps link, and is resumable after interruption (a page token).
  - [ ] A pull is idempotent: re-applying the same page changes nothing (`inbound_changes`).
  - [ ] Visibility property test: with 8 concurrent writers to `change_log` and a reader pulling continuously, the reader eventually sees every row and never skips one (with the chosen approach).
  - [ ] A pulled change for an aggregate with a pending local edit is held (`conflict_pending`) and settled by the push result.

### E17-T08 · Link flow (create vs join)
- **Size** M · **Deps** E17-T03, E17-T07 · **PRD** SYN-001, §7.1, D-04, D-10
- **Scope:** Settings → Cloud → Link:
  1. The owner logs in.
  2. Either:
     - **Create cloud business from this register** (the local workspace IDs are kept, and the backlog drains), or
     - **Join an existing business** (only when the local database has no live sales and no catalogue beyond seed data; otherwise the action is disabled with an explanation). Joining wipes the local business tables and bootstraps.
  3. Register the device.
  4. Start the worker.
- **Acceptance:**
  - [ ] Create path: an install offline for 7 days (≈ 3,000 events from the fixture generator) links, and every event is applied. The cloud reports equal the device reports.
  - [ ] Join path: register 2 joins and receives the catalogue, settings and staff. Its first sale gets the receipt numbers for its own register code (`R02-000001`).
  - [ ] Linking is audited, and only the owner can link.
  - [ ] Losing the network mid-link leaves a resumable `linking` state. No partial wipe: the join wipe happens only after the bootstrap has been fully downloaded to a staging area.

### E17-T09 · Sync status and repair queue UI
- **Size** S · **Deps** E17-T03 · **PRD** SYN-007, AT-13, §5.1
- **Acceptance:**
  - [ ] The status bar indicator states are: Not linked · Synced (last at HH:MM) · Sync pending (N) · Offline (N pending) · Attention needed (conflicts/rejections) · Device revoked · Update required.
  - [ ] The sync page shows the last successful push and pull, pending count, conflicts, rejected events with the reason code and a plain-language explanation, and actions: retry now, view payload (owner), export for support.
  - [ ] The sale completion screen and the sale detail show `Saved on this device · Sync pending` until the event is applied, then `Synced` (AT-13).

### E17-T10 · Network fault-injection harness and replay suite (stage exit)
- **Size** M · **Deps** E17-T03..T08 · **PRD** §16.2, §16.3, AT-06
- **Scope:**
  - A `FaultyTransport` wrapper and a TCP proxy mode for e2e. The scenarios are: drop request, drop response (server applied!), duplicate request, delay, reorder batches, 500/503, connection reset mid-body, and flapping online/offline during checkout and during sync.
- **Acceptance:**
  - [ ] **AT-06:** 200 sales made offline, then reconnected under each fault scenario. The cloud then has exactly 200 sales, the correct tender totals and the correct stock movements. `event_inbox` shows a single application per event, even with clients retrying the same event many times.
  - [ ] A forced client kill during a push, followed by a restart, converges with no duplicates.
  - [ ] Online/offline flapping during checkout never blocks or fails a local sale.
  - [ ] It runs nightly in CI (the full matrix) and per PR (smoke subset).

## E18 Conflict resolution

### E18-T01 · Client conflict store and field diff
- **Size** S · **Deps** E17-T03, E17-T06 · **PRD** SYN-010, §11.4
- **Acceptance:**
  - [ ] A conflict row stores the local and server payloads and versions. It computes a field-level diff for product (name, SKU, price, cost, tax category, barcodes per variant, status), category, tax rate, setting and staff.
  - [ ] The aggregate is marked `sync_status = conflict`. Further local edits are allowed and are superseded into the same conflict.

### E18-T02 · Conflict resolution UI and resolution events
- **Size** M · **Deps** E18-T01 · **PRD** §11.4, AT-07
- **Acceptance:**
  - [ ] **AT-07:** register A and register B edit the price of the same product offline. Both reconnect. One is applied and the other shows a conflict listing local versus server for each field. Nothing is silently overwritten.
  - [ ] Resolution choices per field: keep local, keep server, or a custom value. Submitting creates a `product.upserted` with `base_version = server_version`, applies it locally, and audits `sync.conflict_resolved` with the choices.
  - [ ] A barcode collision offers "remove from this product" or "move to this product" (the latter is a two-aggregate resolution and needs ConflictResolve).
  - [ ] An unresolved conflict never blocks checkout. The local value continues to be used, and a badge is shown.

## E14 Reports (cloud scope)

### E14-T05 · Consolidated cloud reports
- **Size** M · **Deps** E17-T05 · **PRD** REP-008 (P1, minimal version pulled in for §10.1 and §15 labelling)
- **Acceptance:**
  - [ ] `GET /v1/reports/sales` returns rollups by day, register and tender, with `completeness` per register.
  - [ ] Desktop reports offer the scope "All registers (cloud)" when online, with the label "Last synced HH:MM — N registers have unsynced data", or "Fully synchronized" when every register's ack is current within 5 min (PRD §15).
  - [ ] Offline, the cloud scope shows the last cached result labelled **stale** (§11.6).
