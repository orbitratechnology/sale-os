# 02 — Architecture and Modules

This document refines PRD §8. It sets the repository layout, what each module owns, the
dependency rules, and how a command flows through the system.

## 1. Repository layout

```text
/apps
  /desktop-ui              React + TypeScript + Vite (pnpm workspace)
    /src/ipc               GENERATED bindings (tauri-specta) — do not edit
    /src/features          checkout, catalogue, inventory, sales, reports, settings, auth, sync, devtools
    /src/components        shared UI primitives (keypad, money display, dialogs, status bar)
    /src/lib               formatting (money/qty from strings), keyboard map, scan capture
  /desktop-tauri           Tauri 2 entry point, IPC command adapters, AppServices wiring, workers
  /cloud-api               Axum service (--role api | --role worker)
/crates
  /pos-domain              Pure domain: types, rules, calculations. No IO, no async.
  /pos-application         Use cases, permissions, WriteSet, port traits (store, queries, clock, ids, devices)
  /pos-local-db            SQLite implementation of store + query ports; migrations; backup
  /pos-cloud-db            PostgreSQL repositories, migrations, RLS session helpers
  /pos-sync                Event envelope, event payload types, schema versioning, sync protocol DTOs, conflict types
  /pos-device-ports        Barcode / receipt-output / payment / cash-drawer port traits and event types
  /pos-device-virtual      Virtual scanner, virtual printer, mock payment provider, network fault injector
  /pos-receipts            ReceiptDocument model, builders, PDF + ESC/POS + plain-text renderers
  /pos-observability       JSON logging with secret redaction and card-number masking (desktop + cloud)
  /pos-testkit             Builders, fixtures, golden-file helpers, fake clock/ids, in-memory transport
/contracts
  /openapi                 pos-cloud.v1.yaml (source of truth for REST)
  /events                  JSON Schema per event type + version (generated from pos-sync, checked in CI)
/tests
  /fixtures                catalogue CSVs (20 / 5k / 50k variants), pricing golden scenarios, receipt goldens
  /integration             cross-crate Rust tests (desktop core ↔ cloud-api with real Postgres via testcontainers)
  /e2e                     Playwright against the Tauri app (WebDriver) using virtual devices
/xtask                     cargo xtask: db-reset, seed, gen-bindings, gen-schemas, bench, crash-harness
/docs                      PRD, ADRs, this backlog
```

## 2. Module responsibilities

| Module | Owns | Must not |
|---|---|---|
| `pos-domain` | Money, Quantity, RatePpm and ID types. Product aggregate rules. Pricing, tax and discount engine. Cart aggregate. Tender settlement. Return planning. Stock movement derivation. Permission enum and role matrix. Receipt numbering format. | Do IO or async, depend on `sqlx`, `tauri`, `axum`, or read the clock directly |
| `pos-application` | Use-case services (`CheckoutService`, `CatalogService`, …). `CommandContext`, `Authorizer`, approval tokens. `WriteSet` builder and validation. Port traits: `LocalStore`, `*Queries`, `Clock`, `IdGenerator`, `CredentialStore`, `SyncTransport`. Audit and outbox event construction. | Know about SQL, HTTP or the UI |
| `pos-local-db` | SQLite pool (one writer actor, N readers). Migrations. `LocalStore::apply`. Query implementations. FTS maintenance. Backups and integrity checks. | Contain business rules beyond constraints and invariants |
| `pos-sync` | `EventEnvelope`, every event payload type and version, upcasters, the push/pull/bootstrap DTOs, `PushResult` statuses, conflict snapshots | Depend on any database crate |
| `pos-receipts` | `ReceiptDocument`, builders from `SaleSnapshot`/`ReturnSnapshot`, renderers (PDF, ESC/POS, text) | Talk to printers (that is the adapters' job) |
| `pos-device-ports` | Async port traits, `ScanEvent`, `PrintStatus`, `PaymentStatus`, capability profiles | Contain implementations |
| `pos-device-virtual` | Simulators that implement the ports, with a scriptable scenario API | Ship to release builds unless `devtools` is enabled |
| `pos-cloud-db` | Tenant-scoped repositories, inbox, change log, projections, RLS helpers | Accept a `business_id` from anything except the auth principal |
| `apps/desktop-tauri` | Startup and migration orchestration, `AppServices` DI container, the IPC command adapters, sync worker, print worker, auto-lock timer, capability config | Contain business rules |
| `apps/cloud-api` | HTTP routing, auth middleware, tenant session, event appliers (using `pos-domain` validation), reports, worker role | Bypass `pos-domain` validation for ingested events |
| `apps/desktop-ui` | Rendering, input capture (including keyboard-wedge detection), formatting, navigation | Do arithmetic on money, write to storage, or decide permissions (it may only *hide* controls) |

## 3. Dependency rules (enforced in CI by `cargo deny` bans and an `xtask check-deps` graph test)

```text
pos-domain        ← (no internal deps)
pos-sync          ← pos-domain
pos-receipts      ← pos-domain
pos-device-ports  ← pos-domain
pos-application   ← pos-domain, pos-sync, pos-receipts, pos-device-ports
pos-local-db      ← pos-application, pos-domain, pos-sync
pos-device-virtual← pos-device-ports, pos-domain
pos-cloud-db      ← pos-domain, pos-sync
desktop-tauri     ← everything desktop-side
cloud-api         ← pos-domain, pos-sync, pos-cloud-db   (never pos-local-db, never tauri)
pos-observability ← (no internal deps; used by desktop-tauri and cloud-api)
pos-testkit       ← dev-dependency only
```

The PRD's main reuse claim is that the same domain crate runs on the desktop and in the cloud
(§8.1). `cloud-api` therefore validates incoming `sale.completed` events by recomputing the
totals with `pos-domain::pricing::verify_snapshot()` and rejects any mismatch. This uses the
same code that produced the totals on the desktop. It is a check only. The server never
*replaces* desktop totals (PRD §8.3, "sync must not recalculate").

## 4. Command flow: completing a cash sale

```text
React (PaymentPanel)
  └─ ipc.checkoutComplete({ cartId, cartRevision, tenders })           [generated binding]
      └─ desktop-tauri::commands::checkout_complete                    [adapter: deserialize, map errors]
          └─ CheckoutService::complete_sale(ctx, input)                [pos-application]
              1. ctx.authorize(Permission::SaleComplete)
              2. queries.load_cart(cart_id) → Cart (check revision = optimistic UI guard)
              3. queries.load_pricing_context() → tax rates, settings, variants
              4. domain::pricing::price_cart(...) → PricedCart
              5. domain::settlement::settle(total, tenders, policy) → Settlement
              6. domain::inventory::movements_for_sale(...) + negative-stock policy
              7. build SaleSnapshot (immutable lines with name/sku/price/tax snapshots)
              8. pos_receipts::build_sale_receipt(snapshot, receipt_settings) → ReceiptDocument
              9. WriteSet::builder()
                   .insert_sale(snapshot).insert_tenders(..).append_movements(..)
                   .close_cart(cart_id).insert_receipt(doc)
                   .audit(AuditAction::SaleCompleted, ..)
                   .outbox(SaleCompletedV1 { .. })
                   .build()?                                           [rejects missing audit/outbox]
             10. store.apply(write_set)                                [ONE BEGIN IMMEDIATE txn:
                                                                        allocates receipt_seq + local_sequence,
                                                                        writes all rows, updates stock_balances]
             11. print_queue.enqueue_if_auto_print(receipt_id)         [after commit; failure ≠ sale failure]
          ← CompletedSaleDto { saleId, receiptNumber, change, syncState: "pending" }
  UI shows "Saved on this device · Sync pending" (PRD §7.3)
```

Notes:

- The receipt number is allocated inside `apply`. The `ReceiptDocument` therefore holds a
  placeholder that `apply` fills in before the receipt row is written, or the builder is
  called again inside a store callback. `E09-T08` picks one approach and records it.
- Printing happens **after** the commit, through the print queue, so a printer failure can
  never roll back a sale (PRD §11.2, RCP-006).

## 5. Background workers in the desktop process

| Worker | Trigger | Responsibility |
|---|---|---|
| `SyncWorker` | Startup, after commit (notify), connectivity change, backoff timer | Push the outbox, then pull changes, then emit `sync://status` |
| `PrintWorker` | Job enqueued, retry requested by the operator | Submit jobs to the adapter, poll status, move to `unknown` on timeout (never auto-resubmit) |
| `BackupWorker` | Hourly when changes exist, on shutdown, before a migration | `VACUUM INTO` with rotation |
| `CheckpointWorker` | Idle for 60 s | `wal_checkpoint(TRUNCATE)` |
| `LockTimer` | Inactivity setting | Lock the session and emit `session://locked` |

All workers run on the Tokio runtime owned by `desktop-tauri`, take a cancellation token,
and report into `tracing`.

## 6. Frontend feature map (desktop-ui)

| Area (PRD §5) | Routes | Key components |
|---|---|---|
| Checkout | `/checkout` (default) | `ScanCapture`, `CartTable`, `TotalsPanel`, `ProductSearch`, `QuickSelectGrid`, `PaymentPanel`, `ReceiptDialog` |
| Products & Inventory | `/products`, `/products/:id`, `/inventory`, `/inventory/receive`, `/inventory/adjust`, `/import` | `ProductEditor`, `VariantGrid`, `BarcodeList`, `StockOverview`, `ImportWizard` |
| Sales | `/sales`, `/sales/:id`, `/sales/:id/return` | `SalesList`, `SaleDetail`, `ReturnWizard`, `ReceiptPreview` |
| Reports | `/reports/*` | `DateRangePicker`, `ScopeBadge` (device-local / last-synced cloud), `ExportButton` |
| Settings | `/settings/*` | Business, Taxes, Receipt, Staff, Register, Sync, Devices (simulators in dev) |
| Global | — | `StatusBar` (user, register, offline/sync indicator), `LockScreen`, `ApprovalDialog` (manager PIN) |
