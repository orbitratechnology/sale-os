# 07 — Contracts: IPC Commands, Errors, REST API, Sync Events

There are three contract surfaces. Each has one source of truth and is checked in CI:

| Surface | Source of truth | Generated artefact | CI check |
|---|---|---|---|
| Desktop IPC | Rust command signatures + `specta` types | `apps/desktop-ui/src/ipc/bindings.ts` | `xtask gen-bindings --check` |
| Cloud REST | `contracts/openapi/pos-cloud.v1.yaml` | Rust DTOs are hand-written in `pos-sync::api` and validated against the spec by `schemathesis`/`openapi` tests | Spec lint + contract tests (E16-T06) |
| Sync events | Rust payload types in `pos-sync::events` | `contracts/events/<type>.v<n>.schema.json` (`schemars`) | `xtask gen-schemas --check` and compatibility diff (E17-T01) |

---

## 1. Desktop IPC (Tauri commands)

### 1.1 Rules

- Command names are `snake_case` with an area prefix. TypeScript gets camelCase wrappers.
- The **actor is never an argument.** The command reads the unlocked session from Rust state,
  and a command that needs a session fails with `SESSION_LOCKED` if there is none.
- Money and quantity are **decimal strings of minor units / milli-units** (ADR-001, -002).
- Every command returns `Result<T, IpcError>`.
- Rust pushes state changes to the UI as events:
  - `cart://changed`
  - `sync://status`
  - `print://job`
  - `session://locked`
  - `scanner://status`
  - `stock://changed`
- Tauri **capabilities**: the main window may call only the commands below, plus
  `core:event` and `core:window` basics. The `fs`, `shell`, `http` and `sql` plugins are
  **not** enabled. File saving (PDF) opens the system "Save as" dialog **from Rust**
  (`tauri-plugin-dialog`, no `dialog:*` permission granted to the WebView) and Rust writes
  the file, so the WebView never names a path. CSV exports still go to `<app data>/exports/`.

### 1.2 Role → permission matrix (enforced by `role_grants`; covered by test E05-T03)

| Permission | Owner | Manager | Cashier | Notes |
|---|:-:|:-:|:-:|---|
| SaleComplete, CartClear, SalesView, CatalogView, InventoryView, CashSessionOpen | ✓ | ✓ | ✓ | |
| CartLineDiscount, CartOrderDiscount | ✓ | ✓ | ✓ | A cashier is limited to `discount.cashier_max_ppm` (default 10%). Above that, `DiscountAboveThreshold` approval is needed. |
| DiscountAboveThreshold, PriceOverride, CartVoid (cart with lines) | ✓ | ✓ | approval | |
| ExternalTenderRecord | ✓ | ✓ | ✓ | Configurable: `payments.cashier_external_tender` |
| ReceiptReprint | ✓ | ✓ | ✓ | Every reprint is audited and marked as a REPRINT copy |
| ReturnCreate | ✓ | ✓ | approval | `returns.cashier_requires_approval` (default true) |
| ReturnApprove | ✓ | ✓ | – | |
| CatalogEdit, BarcodeCreateFromScan, CatalogImport, CatalogExport | ✓ | ✓ | – | |
| InventoryReceive | ✓ | ✓ | – | |
| InventoryAdjust | ✓ | ✓ | – | |
| CashSessionClose, CashSessionViewVariance | ✓ | ✓ | close only (blind count) | |
| ReportsView, ReportsExport | ✓ | ✓ | – | |
| AuditView, SettingsEdit, StaffManage, DeviceLink, SyncManage | ✓ | – | – | |
| ConflictResolve | ✓ | ✓ | – | |
| DevtoolsUse | debug builds only | | | |

"approval" means the command fails with `APPROVAL_REQUIRED` unless the call carries a valid
`approvalTokenId` from an approver who holds the permission.

### 1.3 Command catalogue

| Command | Permission | Input → Output | Task |
|---|---|---|---|
| `app_bootstrap_state` | none | `→ { setupComplete, mode, linkState, register?, business? }` | E04-T01 |
| `setup_create_workspace` | none (only while setup is incomplete) | `{ business, location, register, owner:{name,pin}, taxDefaults } → WorkspaceDto` | E04-T01 |
| `setup_set_mode` | SettingsEdit | `{ mode:'sandbox'\|'live' } → ()` | E04-T02 |
| `auth_list_users` | none | `→ [{ userId, displayName, role }]` (active users only) | E05-T02 |
| `auth_login` | none | `{ userId, pin } → SessionDto` | E05-T02 |
| `auth_logout` / `auth_lock` | session | `→ ()` | E05-T05 |
| `auth_unlock` | none | `{ pin } → SessionDto` (the same user, or a manager who takes over) | E05-T05 |
| `auth_request_approval` | session | `{ approverUserId, pin, scope } → { approvalTokenId, expiresAt }` | E05-T04 |
| `staff_list` / `staff_create` / `staff_update` / `staff_deactivate` / `staff_reset_pin` | StaffManage | … | E05-T01 |
| `categories_list` / `category_save` / `category_archive` | CatalogView / CatalogEdit | Tree at most 3 levels, no cycles (`VALIDATION_FAILED`, `details.field = parent`); archiving needs no active subcategories; versioned | E06-T05 |
| `tax_config_get` / `tax_rate_save` / `tax_rate_archive` / `tax_category_save` | SettingsEdit (owner) | Rates with `from` (inclusive) / `to` (exclusive) business dates; same-name active rates may not overlap; a used rate cannot be archived (`details.value` lists the categories); exactly one default category (setting a new default moves the flag) | E06-T05 |
| `settings_get` / `settings_update` | session / SettingsEdit | `{ key, value, baseVersion }` | E04-T03 |
| `business_get` / `business_update` | session / SettingsEdit | | E04-T03 |
| `tax_list` / `tax_rate_upsert` / `tax_category_upsert` | CatalogView / SettingsEdit | | E06-T05 |
| `category_list` / `category_upsert` / `category_archive` | CatalogView / CatalogEdit | | E06-T05 |
| `product_list` | CatalogView | `{ filter, page } → Paged<ProductRow>` | E06-T06a |
| `product_get` | CatalogView | `{ productId } → ProductDto` | E06-T02 |
| `product_create` / `product_update` | CatalogEdit | `{ product, baseVersion? } → ProductDto` | E06-T02 |
| `product_archive` | CatalogEdit | `{ productId, baseVersion } → ()` | E06-T02 |
| `catalog_lookup_code` | CatalogView | `{ code } → { kind:'barcode'\|'sku', variant } \| null` | E06-T04 |
| `catalog_search` | CatalogView | `{ query, limit } → [SearchHit]` | E06-T04 |
| `catalog_quick_select` | CatalogView | `→ [SellableVariantDto]` | E09-T04 |
| `import_products_open` | CatalogImport | `→ { jobId, fileName, encoding, delimiter, header, sample, totalRows, mapping } \| null`: Rust opens the system "Open" dialog and reads the file (the WebView never names a path); null when cancelled | E06-T07 |
| `import_products_validate` | CatalogImport | `{ jobId, mapping: [{ column, index }], onDuplicate: skip\|update\|error } → { totalRows, validRows, errorRows, duplicateRows, skippedRows, productsToCreate, productsToUpdate, problems: [{ line, status, messages }] }` (first 200 problems); repeatable until committed | E06-T07 |
| `import_products_commit` | CatalogImport | `{ jobId } → { productsCreated, productsUpdated, rowsCommitted }`; 500 products per transaction, each with one `catalog.import_committed` audit row; run again to resume | E06-T07 |
| `import_products_error_report` | CatalogImport | `{ jobId } → { path, fileName } \| null`: the rows not imported with an `error` column, saved through the system dialog | E06-T07 |
| `import_products_cancel` | CatalogImport | `{ jobId } → ()` before committing | E06-T07 |
| `scan_submit` | SaleComplete | `{ scanEventId, raw, source, symbologyHint? } → ScanOutcome` (`Added{line} \| Incremented{line} \| NotFound{code} \| Duplicate \| Invalid{reason}`) | E07-T02 |
| `cart_current` | session | `→ CartView` (cart and `PricedCart`) | E09-T01 |
| `cart_add_variant` | SaleComplete | `{ variantId, qty?, via } → CartView` | E09-T01 |
| `cart_set_quantity` / `cart_remove_line` | SaleComplete | `→ CartView` | E09-T01 |
| `cart_set_line_discount` / `cart_set_order_discount` | CartLineDiscount / CartOrderDiscount (+ approval) | `{ …, discount (with `reason` above the limit), approvalTokenId? } → CartView` | E08-T02 |
| `cart_override_price` | PriceOverride (+ approval) | `{ lineId, unitPriceMinor, reason, approvalTokenId? } → CartView` | E08-T02 |
| `cart_clear` | CartClear | `{ revision } → CartView` | E09-T01 |
| `cart_void` | CartVoid | `{ reason, revision } → ()` | E12-T04 |
| `cart_recovery_state` / `cart_discard_recovered` | session | `→ { recovered: CartView? }` | E09-T02 |
| `checkout_complete` | SaleComplete (+ ExternalTenderRecord when needed) | `{ cartId, revision, tenders:[TenderInput] } → CompletedSaleDto` | E09-T08 |
| `sales_list` / `sales_get` | SalesView | `{ filter: { from?, to?, receipt?, cashierUserId?, tender? }, after?, limit? } → { rows, next? }`; `{ saleId } → SaleDetailDto`. Finding by receipt number is the `receipt` filter (full number or its digits), and `return_find` for returns, so there is no separate `sales_find_by_receipt` | E12-T03 |
| `return_preview` | ReturnCreate | `{ saleId, lines } → ReturnPlanDto` | E12-T01 |
| `return_create` | ReturnCreate (+ approval) | `ReturnRequest → ReturnDto` | E12-T02 |
| `inventory_overview` / `inventory_history` | InventoryView | | E11-T05 |
| `inventory_post_document` | InventoryReceive (opening/receipt) / InventoryAdjust (adjustment/write_off) | `InventoryDocumentDraft → DocumentDto` | E11-T02/T03 |
| `cash_session_current` / `cash_session_open` / `cash_session_close` | see matrix | | E15-T02 |
| `cash_session_z_get` / `cash_session_z_print` | CashSessionViewVariance | `{ sessionId? }` (null: the last closed session) | E15-T02 |
| `stock_overview` / `stock_history` | InventoryView | `{ lowOnly }` → the stock report rows; `{ variantId, limit }` → movements newest first with type, delta, bucket, source number, user, time | E11-T05 |
| `settings_get` / `settings_update` | SettingsEdit (owner) | `→ [{ key, group, label, help, kind, valueJson, defaultJson, version }]`; `{ key, valueJson, baseVersion } → setting` (unknown key or wrong type: `VALIDATION_FAILED`; stale version: `CONFLICT_VERSION`). Registry and defaults: docs/settings.md (generated) | E04-T03 |
| `business_profile_get` / `business_profile_update` | SettingsEdit (owner) | `→ BusinessProfileDto` (name, legal name, tax registration no., address lines, phone, email, this till's name; country, time zone, currency and tax mode read-only); `{ input, businessVersion, registerVersion } → BusinessProfileDto`. Only what changed is written; `business.updated` (audit metadata: the names of the changed fields) and `register.upserted` (audit `register.updated` from/to). Invalid input: `VALIDATION_FAILED` with `details.field` (and `details.value` = the maximum length) | E04-T03 |
| `app_preferences` / `auth_lock` | session | Scanner suffix and key gap, idle-lock seconds; `auth_lock` ends the session for inactivity (`auth.session_locked`) | E04-T03 |
| `audit_list` | AuditView (owner) | `{ filter: { action? (exact, or a group ending in `.`), userId? (actor or approver), from?, to? (business dates) }, before?, limit? } → { rows: [{ localTime, action, actorName, approverName, entityType, entityId, entityLabel, metadata }], next }` | E15-T01 |
| `stock_verify` | InventoryView | `→ [{ variantId, bucket, recordedMilli, fromMovementsMilli }]` (empty is healthy) | E11-T06 |
| `stock_rebuild` | SettingsEdit (owner) | `→` the drift that was repaired; audited `inventory.projection_rebuilt` | E11-T06 |
| `receipt_get_document` | SalesView | `{ source, id } → ReceiptDocument` | E13-T03 |
| `receipt_export_pdf` | SalesView | `{ saleId \| returnId, paper: receipt\|a4 } → { path, fileName } \| null` (null: the user cancelled the save dialog); audited `receipt.exported_pdf` | E13-T04 |
| `receipt_print` / `receipt_reprint` | SalesView / ReceiptReprint | `{ receiptId } → PrintJobDto` | E13-T06 |
| `print_jobs_active` / `print_job_resolve_unknown` | session | `{ jobId, outcome:'printed'\|'not_printed' } → ()` | E13-T06 |
| `reports_sales_summary` / `reports_tender_mix` / `reports_top_products` / `reports_stock` | ReportsView | `{ range: { from, to } }` (+ `by`, `limit` for top products; `lowOnly` for stock) → … + `scope: { scope: 'device', asOfMs }`. The `scope: 'cloud'` argument arrives with Stage 3. Definitions: docs/reporting-definitions.md | E14 |
| `export_csv` | ReportsExport / CatalogExport (products, stock) | `{ dataset, range? } → { path, fileName, rows }`. Saved under `<app data>/exports/` (never overwriting) until a native save dialog exists; datasets `transactions`, `tenders`, `returns`, `products`, `stock`, `sales_summary`, `tender_mix`, `top_products` | E14-T04 |
| `audit_list` | AuditView | | E15-T01 |
| `sync_status` | session | `→ SyncStatusDto { linkState, pending, rejected, conflicts, lastPushOkAt, lastPullOkAt, lastError?, clockSkewWarning }` | E17-T09 |
| `sync_link_start` / `sync_link_complete` | DeviceLink | Owner email + password → link result | E17-T08 |
| `sync_retry_now` | session | | E17-T03 |
| `sync_conflicts_list` / `sync_conflict_resolve` | ConflictResolve | `{ conflictId, choice: per-field 'local'\|'server'\|value } → ()` | E18-T02 |
| `sync_repair_queue` / `sync_repair_action` | SyncManage | | E17-T09 |
| `backup_now` / `backup_list` / `backup_restore` | SettingsEdit (owner) | | E19-T03 |
| `diagnostics_export` | session | `{ saveTo } → ()` | E19-T07 |
| `dev_scanner_*`, `dev_printer_*`, `dev_payment_*`, `dev_network_*` | DevtoolsUse (debug builds) | Scenario controls | E07-T03, E13-T06, E10-T02, E17-T10 |

## 2. Error model

```ts
// IPC
type IpcError = { code: ErrorCode; message: string; details?: Record<string, unknown>; retryable: boolean };
```

```json
// REST: RFC 9457 problem+json
{ "type": "https://errors.sale-os.dev/SYNC_OUT_OF_ORDER", "title": "Event out of order", "status": 409,
  "code": "SYNC_OUT_OF_ORDER", "detail": "Expected local_sequence 41, got 43", "expected": 41 }
```

`message` is for logs. The UI maps `code` to localized, actionable copy: what happened, plus
the action available (PRD §5.1).

| Code | Meaning | UI action |
|---|---|---|
| `SESSION_LOCKED` | No unlocked session | Show the lock screen |
| `AUTH_PIN_INVALID` | Wrong PIN (`details.remainingAttempts`) | Retry |
| `AUTH_LOCKED_OUT` | Throttled (`details.retryAt`) | Countdown |
| `PERMISSION_DENIED` | The role lacks the permission | Explain; offer manager sign-in |
| `APPROVAL_REQUIRED` | Approval needed (`details.permission`, `details.scope`) | Open the `ApprovalDialog` |
| `APPROVAL_INVALID` | Token expired, already used, or out of scope | Re-request |
| `VALIDATION_FAILED` | Field errors (`details.fields`) | Inline errors |
| `CATALOG_DUPLICATE_BARCODE` | `details.owner` | Link to the owning product |
| `CATALOG_DUPLICATE_SKU` | | |
| `CATALOG_NOT_FOUND` | | |
| `SCAN_NOT_FOUND` | Unknown code (BAR-006) | Search / create product (if permitted) |
| `SCAN_INVALID` | Malformed input | Re-scan |
| `CART_REVISION_MISMATCH` | The UI is stale | Refresh the cart and show it again |
| `CART_EMPTY` | | |
| `PRICING_DISCOUNT_EXCEEDS` | | |
| `TENDER_INSUFFICIENT` | `details.remaining` | |
| `TENDER_CHANGE_FROM_NON_CASH` | | |
| `TENDER_OVERPAY_NON_CASH` | | |
| `STOCK_NEGATIVE_BLOCKED` | Policy is Block (`details.variants`) | |
| `RETURN_QTY_EXCEEDS` | `details.available` | |
| `RETURN_REFUND_MISMATCH` | | |
| `CASH_SESSION_REQUIRED` | Sale attempted without an open session (if the setting is on) | Open a session |
| `CASH_SESSION_ALREADY_OPEN` | | |
| `PRINT_ADAPTER_UNAVAILABLE` | | Offer PDF instead |
| `PRINT_UNKNOWN_OUTCOME` | | Ask: printed or not? Explicit reprint |
| `STORE_BUSY` | Retryable | Automatic retry ×3, then show |
| `STORE_IO` | Disk or permission error | Diagnostics; the sale is NOT saved |
| `STORE_CORRUPT` | Integrity failure | Stop and guide to restore |
| `STORE_SCHEMA_TOO_NEW` | The database was written by a newer app version (downgrade guard, E02-T02) | Update the app. The file is not modified. |
| `SYNC_NOT_LINKED` | | Link wizard |
| `SYNC_DEVICE_REVOKED` | | Contact owner; local-only mode |
| `SYNC_UPGRADE_REQUIRED` | `details.minVersion` | Update prompt |
| `SYNC_OUT_OF_ORDER` | Server answer (REST only) | Automatic resend |
| `CONFLICT_VERSION` | Master data conflict | Conflict UI |
| `INTERNAL` | A bug | Diagnostics bundle |

## 3. Cloud REST API v1

- Base URL: `https://api.<domain>/v1`. TLS only, HSTS.
- Authentication:
  - The device uses a Bearer access JWT (ADR-013).
  - The owner uses a Bearer user JWT, only for `/auth/*`, `/businesses`, `/devices/register`
    and `/devices/*/revoke`.
- The tenant always comes from the token claims. **No `business_id` in a request body is
  trusted** (PRD §10.2).
- Every request is validated against `X-App-Version`. The server answers `426` with
  `SYNC_UPGRADE_REQUIRED` when the version is below the policy minimum (E19-T09).

| Method & path | Auth | Request → Response | Notes |
|---|---|---|---|
| `GET /v1/health` | none | `→ { status, version }` | |
| `GET /v1/version-policy` | none | `→ { minSupported, latest, updateUrl }` | |
| `POST /v1/auth/signup` | none | `{ email, password } → { accountId }` | Rate-limited |
| `POST /v1/auth/login` | none | `{ email, password } → { userAccessToken, expiresIn }` | |
| `POST /v1/businesses` | user | `{ businessId, name, countryCode, timezone, currencyCode, currencyExponent, pricesIncludeTax } → 201 BusinessDto` | Client-generated ID. Idempotent when the same owner sends the same ID. `409` when another account owns that ID. |
| `GET /v1/businesses` | user | `→ [BusinessDto]` | For the "join existing" path (D-10) |
| `POST /v1/devices/register` | user | `{ businessId, registerId, locationId, installationId, appVersion, osInfo } → { deviceId, accessToken, refreshToken, expiresIn }` | The user must be an owner of the business. Revokes any previous active device on this register (after confirmation in the UI). |
| `POST /v1/auth/device/refresh` | refresh token | `{ refreshToken } → { accessToken, refreshToken, expiresIn }` | Rotation. Reuse ⇒ the device is revoked (`401 SYNC_DEVICE_REVOKED`). |
| `POST /v1/devices/{deviceId}/revoke` | user | `→ 204` | |
| `GET /v1/devices` | user | `→ [DeviceDto]` | |
| `GET /v1/sync/bootstrap?page=` | device | `→ { snapshotCursor, entities:{ business, locations, registers, settings, staff, taxRates, taxCategories, categories, products }, nextPage? }` | Paged by about 2,000 aggregates. `snapshotCursor` is the change_log position taken at the start of the snapshot. |
| `GET /v1/sync/changes?cursor=&limit=` | device | `→ { changes:[Change], nextCursor, hasMore }` | `Change = { serverSeq, aggregateType, aggregateId, version?, kind, originRegisterId?, snapshot }` |
| `POST /v1/sync/events` | device | `PushRequest → PushResponse` (see below) | Up to 200 events or 2 MB per request |
| `GET /v1/sync/ack` | device | `→ { registerId, ackSequence, serverTime }` | `registerId` comes from the token. The query parameter in the PRD is dropped as redundant. |
| `GET /v1/catalog/products?query=&page=` | user | `→ Paged<ProductDto>` | Admin read-only in the MVP |
| `POST /v1/catalog/products` | user | `{ product, expectedVersion } → ProductDto \| 409 CONFLICT_VERSION` | Present in the PRD. Implemented through the same applier as device events (origin = cloud). P1 UI. |
| `GET /v1/reports/sales?from=&to=&groupBy=day\|register\|tender` | user or device | `→ { rows, asOf, completeness:{ registers:[{ registerId, lastAckAt, pendingKnown }] } }` | `completeness` drives the "fully synchronized vs last-synced" label (PRD §15) |

### Push contract

```json
// POST /v1/sync/events
{
  "registerId": "0190f0c2-…",          // must equal token claim, else 403
  "events": [
    {
      "eventId": "0190f0d9-…",
      "businessId": "…", "locationId": "…", "registerId": "…",
      "localSequence": 41,
      "eventType": "sale.completed",
      "schemaVersion": 1,
      "aggregateType": "sale", "aggregateId": "…",
      "baseVersion": null,
      "occurredAt": "2026-11-05T09:14:03.120Z",
      "payload": { … }
    }
  ]
}
// 200
{
  "ackSequence": 41,
  "serverTime": "2026-11-05T09:20:00.004Z",
  "results": [
    { "eventId": "…", "localSequence": 41, "status": "applied", "serverSeq": 99812 },
    // status ∈ applied | duplicate | conflict | rejected | out_of_order
    // conflict: { serverVersion, serverSnapshot, reason }   rejected: { code, detail }
  ]
}
```

## 4. Sync event catalogue (`pos-sync::events`)

### Envelope (all events)

`eventId, businessId, locationId, registerId, localSequence, eventType, schemaVersion,
aggregateType, aggregateId, baseVersion?, occurredAt, payload` (PRD §11.3).

### Versioning rules (E17-T01)

- **Adding** an optional payload field is a minor change. The schema version stays the same,
  and older servers ignore the field.
- **Removing or renaming** a field, or changing its meaning, creates a new `schemaVersion`.
  The server keeps appliers for the previous version, or an upcaster, for at least 2 app
  releases.
- If the server receives a `schemaVersion` above what it supports, it answers `rejected` with
  `EVENT_VERSION_UNSUPPORTED`. The client then shows "update the server/app" guidance and
  **keeps the event**. Unknown versions are never silently dropped.

### Master-data events (versioned aggregates, ADR-011)

| Event type | Aggregate | Payload (summary) |
|---|---|---|
| `business.updated` v1 | business | Full profile snapshot |
| `location.upserted` v1 | location | Snapshot |
| `register.upserted` v1 | register | Snapshot |
| `setting.updated` v1 | setting (key) | `{ key, value }` |
| `staff.upserted` v1 | staff_user | `{ userId, displayName, role, pinHash, status }` |
| `tax_rate.upserted` v1 | tax_rate | Snapshot |
| `tax_category.upserted` v1 | tax_category | Snapshot including `rateIds` |
| `category.upserted` v1 | category | Snapshot |
| `product.upserted` v1 | product | Full aggregate: product, variants[], barcodes[], status. An archive is an upsert with `status = archived`. |

### Fact events (immutable, never conflict; validated, may be `rejected`)

| Event type | Payload (summary) |
|---|---|
| `sale.completed` v1 | The whole sale snapshot: header, lines (with snapshots and tax lines), tenders, stock movements, receipt number, mode. **Self-contained.** The server never looks up current prices (PRD §8.3). |
| `tender.status_changed` v1 | `{ tenderId, saleId, from, to, source, detail }` |
| `return.completed` v1 | Return header, lines, refunds, stock movements, original sale ID |
| `inventory.document_posted` v1 | Document header and movements |
| `cash_session.opened` v1 / `cash_session.closed` v1 | Session fields |
| `audit.recorded` v1 | Standalone audit entries that are not embedded in another fact (login failures, lockouts, approvals, reprints, voids, exports) |

Server-side validation for facts (they are `rejected` if any check fails):

- The schema validates.
- The referenced register and location belong to the principal.
- `verify_snapshot` totals match.
- Quantities are positive.
- Tender sum = total.
- For returns: the original sale exists, or is pending from the same register (if it is
  missing, the server answers `transient: ORIGINAL_SALE_PENDING`). Over-return across
  registers is **accepted and flagged**, not rejected. It is a business fact that already
  happened.

## 5. Audit actions (catalogue, E15-T01)

```text
auth.login, auth.login_failed, auth.locked_out, auth.logout, auth.session_locked, auth.approval_granted,
staff.created, staff.updated, staff.deactivated, staff.pin_reset,
settings.updated, business.created, business.updated, location.upserted, register.updated, mode.changed,
catalog.product_created, catalog.product_updated, catalog.product_archived, catalog.import_committed, catalog.category_saved,
tax.config_updated,
cart.cleared, cart.voided, cart.price_overridden, cart.discount_applied, cart.recovered, cart.recovery_discarded,
sale.completed, tender.recorded_external, tender.status_changed,
return.completed, refund.recorded,
inventory.opening_posted, inventory.receipt_posted, inventory.adjustment_posted, inventory.write_off_posted, inventory.negative_flagged, inventory.projection_rebuilt,
receipt.reprinted, receipt.exported_pdf, print.unknown_resolved,
cash_session.opened, cash_session.closed,
report.exported, data.exported, backup.created, backup.restored,
sync.linked, sync.device_revoked, sync.conflict_resolved, sync.event_rejected
```
