# Stage 1 — Catalogue and Checkout Foundation

**Goal.** A cashier can sign in, find products by barcode, SKU, name or quick-select, build a
cart with discounts and taxes, and see correct totals. Everything works locally, with no cloud
and no hardware. Sale *completion* lands in Stage 2. Stage 1 ends with a priced cart.

**Exit gate (PRD §17).** Fast local checkout in demo mode, with no cloud and no physical
hardware. It is measured by:

- scan → line visible p95 < 100 ms with `shop5k` seeded (NFR-add-to-cart)
- name search p95 < 150 ms with `bench50k` (NFR-search)
- the AT-02 and AT-03 e2e tests are green

**Rough size.** ≈ 85–105 engineer-days.

**Decisions needed by this stage:** D-04 (at the start), D-02 (by the end).

---

## E04 Business setup and configuration

### E04-T01 · Workspace creation (business, location, register, owner)
- **Size** M · **Deps** E02-T03, E05-T02 · **PRD** SET-001, SET-002, §7.1
- **Scope:**
  - Complete migration `0001_core.sql` (`businesses`, `locations`, `registers`, `device_identity`, `business_settings`).
  - `SetupService::create_workspace` runs as **one WriteSet**. It creates the business, the location, the register (code `R01`), the owner staff user, default tax category and rates (from wizard input), default settings, counters, audit entries and outbox events (ADR-009).
  - Implement `app_bootstrap_state`.
- **Acceptance:**
  - [ ] Works with the network adapter disabled (AT-01 precondition).
  - [ ] `setup_create_workspace` called a second time returns `PERMISSION_DENIED`, since setup is complete.
  - [ ] Validation:
    - IANA timezone
    - ISO-3166 country
    - ISO-4217 currency, with the exponent taken from an embedded ISO table and not from user input
    - business name required
    - PIN of 4–8 digits
  - [ ] The outbox has `business.updated`, `location.upserted`, `register.upserted`, `staff.upserted`, `tax_rate.upserted`, `tax_category.upserted` and `setting.updated` events, all with sequential `local_sequence` values starting at 1.

### E04-T02 · First-run wizard UI
- **Size** M · **Deps** E04-T01, E09-T03 · **PRD** §7.1, SET-001/002, D-04
- **Scope:** The steps are:
  1. Business profile and logo. The logo is copied to app data, resized to at most 512 px, and stored as PNG.
  2. Country, timezone and currency, with defaults from the OS locale.
  3. Tax defaults: inclusive or exclusive, and up to 3 rates.
  4. Owner name and PIN.
  5. Optional cashiers.
  6. "Add products now / import CSV / later".
  7. Receipt preview, linked to the Stage 2 receipt settings.
  8. **Sandbox mode** until the owner switches to Live (PRD §7.1 steps 8–9).

  Show the "cloud not linked — data stays on this device until you link" explainer.
- **Acceptance:**
  - [ ] An e2e test completes the wizard offline in under 3 minutes (scripted).
  - [ ] Sandbox sales show a SANDBOX banner and are excluded from reports (enforced in E14).
  - [ ] Switching to Live needs the owner PIN and is audited (`mode.changed`).
  - [ ] Every step is keyboard-navigable.

### E04-T03 · Settings store and settings UI shell
- **Size** S · **Deps** E04-T01 · **PRD** SET-004, SET-005
- **Scope:**
  - A typed settings registry in Rust (`SettingKey` enum: type, default, permission, scope).
  - `settings_get`/`settings_update` with `baseVersion`.
  - UI pages for Business, Taxes, Inventory policy (allow negative stock, default unit, default `track_stock`), Security (lock timeout, cashier discount limit, return approval), and Scanner (suffix, burst gap).
- **Acceptance:**
  - [ ] An unknown key or a wrongly typed value gives `VALIDATION_FAILED`.
  - [ ] Each update is one WriteSet with an audit event and an outbox event.
  - [ ] The defaults are documented in `docs/settings.md`, which is generated.

## E05 Staff identity and access

### E05-T01 · Staff management backend
- **Size** M · **Deps** E04-T01 · **PRD** IAM-001, IAM-002
- **Scope:**
  - Migration `0002_staff.sql`.
  - `StaffService` create, update, deactivate and reset-PIN.
  - Invariant: at least one active owner always exists.
  - Name uniqueness among active staff (case-insensitive).
- **Acceptance:**
  - [ ] Deactivating or demoting the last owner returns `VALIDATION_FAILED`.
  - [ ] A PIN reset sets `must_change_pin`, and the next login forces a change.
  - [ ] A deactivated user cannot log in. Their historic sales still show their name.
  - [ ] Only an owner can call these commands. Direct IPC calls as a manager or cashier get `PERMISSION_DENIED` (AT-11 pattern).

### E05-T02 · PIN authentication, lockout and sessions
- **Size** M · **Deps** E05-T01 · **PRD** IAM-005, §13, ADR-013
- **Scope:**
  - Argon2id hashing (parameters per ADR-013), with verification on a blocking thread.
  - `staff_login_throttle` with exponential lockout.
  - `staff_sessions`.
  - The session lives in Rust state and is never sent to JS as a bearer secret.
- **Acceptance:**
  - [ ] Hash verify p95 ≤ 300 ms on the reference machine, and ≥ 50 ms (not trivially weak).
  - [ ] After 5 wrong PINs the user is locked for 30 s. The 6th wrong PIN locks for 60 s, and so on, up to a 15 min cap. A correct PIN resets the counter.
  - [ ] Lockouts are audited (`auth.locked_out`).
  - [ ] PINs never appear in logs. The E01-T04 redaction test is extended to login.

### E05-T03 · Permission enforcement and IPC-bypass harness
- **Size** M · **Deps** E05-T02 · **PRD** IAM-003, AT-11
- **Scope:**
  - The `Permission` enum and role matrix ([07 §1.2](../07-contracts.md#12-role--permission-matrix-enforced-by-role_grants-covered-by-test-e05-t03)).
  - `CommandContext::authorize`.
  - A **table-driven test** that, for every IPC command in the catalogue, calls the command handler **directly** (bypassing the UI) as each role. It asserts the outcome is allowed, `PERMISSION_DENIED` or `APPROVAL_REQUIRED`, as the matrix says.
- **Acceptance:**
  - [ ] The test enumerates commands through the specta registry. Adding a command without a matrix entry fails the test.
  - [ ] A cashier calling `inventory_post_document`, `staff_create`, `settings_update` or `return_create` without approval is rejected (AT-11).
  - [ ] UI hiding is **additionally** applied, but no test relies on it.

### E05-T04 · Manager approval tokens
- **Size** S · **Deps** E05-T03 · **PRD** §5.1, §13
- **Acceptance:**
  - [ ] A token is single-use. A second use gives `APPROVAL_INVALID`.
  - [ ] It expires after 120 s (fake clock).
  - [ ] It is scoped to the permission plus the entity ID plus a maximum amount. Using it for a different cart, or for a larger discount, is rejected.
  - [ ] The approver must hold the permission. Self-approval by a cashier is impossible.
  - [ ] The approval is recorded as `approver_user_id` on the resulting records and in `auth.approval_granted`.

### E05-T05 · Inactivity lock
- **Size** S · **Deps** E05-T02 · **PRD** IAM-005
- **Scope:**
  - A Rust `LockTimer`, reset by any command and by a throttled UI `activity_ping`.
  - It locks the session and emits `session://locked`.
  - The UI shows the lock screen. **The cart is preserved.**
- **Acceptance:**
  - [ ] With timeout = 60 s (fake clock), no activity ⇒ every command returns `SESSION_LOCKED`.
  - [ ] Unlocking as the same user resumes the same cart.
  - [ ] Unlocking as a different cashier is allowed (`switch_user`), and the cart is kept but reassigned. The reassignment is audited.

### E05-T06 · Login, lock and staff management UI
- **Size** M · **Deps** E05-T02..T05
- **Acceptance:**
  - [ ] PIN pad usable by mouse, touch and keyboard.
  - [ ] Shows remaining attempts and a lockout countdown.
  - [ ] Staff list with role badges, plus create, edit, deactivate and reset PIN.
  - [ ] The `ApprovalDialog` component can be reused by any command that returns `APPROVAL_REQUIRED`, and retries the original command with the token.

## E06 Catalogue

### E06-T01 · Catalogue migration
- **Size** M · **Deps** E04-T01 · **PRD** CAT-002..007, §9.2
- **Scope:** Migration `0003_catalogue.sql` exactly as in [04](../04-local-database.md#0003_cataloguesql), plus the units seed and the FTS table.
- **Acceptance:**
  - [ ] The schema test checks the unique indexes: the barcode index on `(business_id, value_normalized)` for active rows, the SKU index (case-insensitive) for active rows, and one default variant per product.
  - [ ] A migration upgrade test from 0002 passes.

### E06-T02 · Product aggregate and commands
- **Size** M · **Deps** E06-T01, E05-T03 · **PRD** CAT-001, -002, -005, -006, -007, ADR-019
- **Scope:**
  - The domain `Product` with validation and version bumps.
  - `CatalogService` create, update and archive. Each is one WriteSet: product, variants, barcodes, FTS rows, audit, and outbox `product.upserted`.
  - A simple product gets a single default variant automatically.
- **Acceptance:**
  - [ ] You can create a simple product (1 default variant), and a variant product with 6 variants (3 sizes × 2 colours), each with its own SKU, price and barcode.
  - [ ] A `service` or `non_stock` product forces `track_stock = false` on every variant.
  - [ ] Unit `ea` rejects fractional `scan_qty`. Unit `kg` accepts 0.250.
  - [ ] Archiving keeps history: past sales still show the product, and search excludes archived items.
  - [ ] An update with a stale `baseVersion` (a local double edit from two windows) gives `CONFLICT_VERSION`.
  - [ ] The FTS row is updated in the same transaction. The test reads FTS immediately after commit.

### E06-T03 · Barcode normalization and uniqueness
- **Size** S · **Deps** E06-T02 · **PRD** CAT-003, CAT-004, BAR-004, §6.2
- **Scope:**
  - `normalize_barcode` per [06 §2](../06-rust-interfaces.md#2-pos-domaincatalog-e06-t02-e06-t03).
  - Check-digit validation for EAN-13, EAN-8, UPC-A and UPC-E.
  - UPC-A and UPC-E are canonicalized to EAN-13 form.
  - `pack` qualifier barcodes carry a pack quantity, for example a case barcode that adds 12.
- **Acceptance:**
  - [ ] Fixture table: `"012345678905"`, `"0012345678905"` and `" 012345678905\r"` all normalize to the same value.
  - [ ] A bad check digit with the symbology hint `ean13` gives `SCAN_INVALID`. With no hint, it is treated as `internal`, accepted and flagged.
  - [ ] Adding a barcode already owned by another active variant gives `CATALOG_DUPLICATE_BARCODE` with the owner. An archived owner's barcode can be reused.
  - [ ] Multiple aliases on one variant all resolve to it.

### E06-T04 · Catalogue queries and performance benchmarks
- **Size** M · **Deps** E06-T03, E00-T04 · **PRD** BAR-004, POS-002, NFR-lookup, NFR-search
- **Scope:**
  - `find_by_barcode`, `find_by_sku`, `search` (FTS5 with a `bm25` rank, plus an exact-SKU boost), `quick_select`, and `catalog_lookup_code`, which tries barcode, then SKU.
  - A `criterion` bench over `bench50k`.
- **Acceptance:**
  - [ ] Barcode lookup p95 < 50 ms and search p95 < 150 ms at 50k variants and 100k barcodes, on CI Windows (results stored as a CI artefact, with a regression alert above 20%).
  - [ ] Search matches a partial name ("coca" finds "Coca-Cola 330ml"), diacritics ("cafe" finds "Café"), SKU prefix and barcode suffix (≥ 4 digits).
  - [ ] Archived products are excluded.

### E06-T05 · Categories and tax configuration
- **Size** S · **Deps** E06-T01 · **PRD** SET-004, CAT-002
- **Acceptance:**
  - [ ] Category tree with a maximum depth of 3. A parent cycle gives `VALIDATION_FAILED`.
  - [ ] Tax rate CRUD with effective dates. Overlapping active periods for the same name are rejected.
  - [ ] A tax category maps to 0..n rates. Exactly one default category is required.
  - [ ] Archiving a rate still used by an active category is blocked, with a list of the affected categories.

### E06-T06a · Product list UI
- **Size** M · **Deps** E06-T04
- **Acceptance:**
  - [ ] A virtualized list stays responsive with 50k rows: scroll at 60 fps and filter results in < 200 ms.
  - [ ] Filters: category, status, stock-tracked, low stock (enabled in Stage 2).
  - [ ] Columns: name, SKU, price, stock, barcode count.

### E06-T06b · Product editor UI
- **Size** M · **Deps** E06-T02, E06-T03
- **Acceptance:**
  - [ ] Create and edit a simple product, and turn it into a variant product by adding option axes.
  - [ ] A variant grid with inline price, SKU, barcode and stock-threshold editing.
  - [ ] A barcode can be added by scanning while the editor is focused (ScanCapture routes to the editor instead of the cart).
  - [ ] Server-side validation errors are shown inline.
  - [ ] Saving shows "Saved on this device".

### E06-T07 · CSV import backend
- **Size** M · **Deps** E06-T02, E06-T03 · **PRD** CAT-008, §16.3
- **Scope:**
  - Migration `0012_import.sql`.
  - Parse with the `csv` crate: UTF-8 (with or without BOM) and Windows-1252 auto-detect; delimiter `,`, `;` or tab auto-detected; files up to 50k rows.
  - The mapping includes variant grouping by a "parent SKU" or "product name" column.
  - Validation per row; duplicate detection within the file and against the existing catalogue; an `onDuplicate` policy of `skip`, `update` or `error`.
  - The commit is chunked into WriteSets of 500 products each, with one `catalog.import_committed` audit event per chunk and a job-level summary.
  - An error report CSV gives the original row plus an error column.
- **Acceptance:**
  - [ ] Fixtures for duplicate barcodes (in-file and existing), missing price, invalid tax category, malformed rows (wrong column count, unterminated quote), negative price and a fractional qty for `ea` each produce the expected per-row errors (§16.3).
  - [ ] Importing 5,000 products takes under 60 s on the reference machine.
  - [ ] A crash halfway through the commit leaves whole chunks committed and the job marked `committing`. Resume continues from the next chunk without creating duplicates (idempotent on `import row → product_id` mapping stored in `parsed_json`).
  - [ ] AT-01 fixture: importing `tests/fixtures/catalogue/demo20.csv` creates 20 products.

### E06-T08 · CSV import wizard UI
- **Size** M · **Deps** E06-T07
- **Acceptance:**
  - [ ] Steps: choose file, preview, map columns (auto-map by header synonyms), validate summary (valid / errors / duplicates), choose duplicate policy, commit with progress, download error report.
  - [ ] Cancelling before commit leaves the catalogue unchanged.

## E07 Barcode input

### E07-T02 · Scan pipeline
- **Size** M · **Deps** E07-T01, E06-T04, E09-T01 · **PRD** BAR-001, -004, -005, -006, -007, §7.2
- **Scope:** `ScanService::submit`:
  1. Deduplicate on `scanEventId` (table `scan_events_seen`). **Never** deduplicate by value or time (BAR-007).
  2. Normalize.
  3. Look up the barcode, then the SKU.
  4. On a match, call `Cart::add` with `scan_qty` or `pack_qty`.
  5. Persist the cart.
  6. Return `ScanOutcome`.

  Manual code entry (BAR-001) uses the same command with `source = manual`.
- **Acceptance:**
  - [ ] The same `scanEventId` delivered twice increments once and returns `Duplicate`.
  - [ ] The same barcode scanned 10 times quickly with distinct IDs gives quantity 10 (AT-03).
  - [ ] A pack barcode (12) followed by a unit barcode of the same variant gives quantity 13 on one line.
  - [ ] A scan when the last line of the same variant has a manual discount appends a new line.
  - [ ] An unknown code returns `NotFound` and does not change the cart.
  - [ ] A scan while locked returns `SESSION_LOCKED`. The scan is not queued silently: the UI shows that the scan was rejected (PRD §5.1, never silently discard a scan).
  - [ ] Latency from IPC call to `cart://changed` emitted: p95 < 30 ms with `shop5k`.

### E07-T03 · Virtual scanner (dev panel and test API)
- **Size** M · **Deps** E07-T02 · **PRD** BAR-003, §12.3, §16.3
- **Scope:**
  - `VirtualScanner` implementing `BarcodeInputPort`.
  - A dev panel (debug builds, `DevtoolsUse`) offering: type or paste a code, choose the suffix, burst N codes with a gap, send an unknown code, send malformed or partial input, duplicate the last delivery, disconnect and reconnect, and pick fixture sets per symbology.
  - The same functions are exposed to e2e tests through `dev_scanner_*` commands.
- **Acceptance:**
  - [ ] Each scenario in PRD §12.3 has an e2e test.
  - [ ] Disconnect shows "Scanner disconnected" in the status bar (from `scanner://status`). Manual entry still works.
  - [ ] Release builds do not contain the dev panel or the commands (a build-config test greps the bindings).

### E07-T04 · Unknown barcode flow
- **Size** S · **Deps** E07-T02, E06-T06b · **PRD** BAR-006
- **Acceptance:**
  - [ ] A not-found toast shows the code and offers **Search** (pre-filled) and **Create product** (only if the user has `BarcodeCreateFromScan`; otherwise "Ask a manager", which opens the approval dialog).
  - [ ] Create product opens the editor with the barcode pre-filled. Saving adds the item to the cart.
  - [ ] An audible or visual error cue is shown; it is configurable and never only colour (a11y).

## E08 Pricing, tax and discounts

### E08-T01 · Tax engine
- **Size** M · **Deps** E03-T03, E06-T05 · **PRD** SET-004, POS-005, ADR-004
- **Scope:**
  - `price_cart` steps 1–3 and 6–9 ([06 §3](../06-rust-interfaces.md#3-pos-domainpricing-e08-t01-e08-t02)) for inclusive and exclusive modes.
  - Multiple rates per category.
  - Effective-date resolution by `business_date`.
  - Tax breakdown aggregation.
- **Acceptance:**
  - [ ] Golden scenarios (E08-T03), each matching to the minor unit:
    - inclusive 15% on 9.99 × 3
    - exclusive 7.25% on 0.333 kg × 12.49/kg
    - zero-rated
    - exempt (no rate)
    - two rates on one line
    - a rate with an effective date starting tomorrow (not applied)
  - [ ] proptest: an inclusive line total equals `round(unit_price × qty) − discounts`, so the customer pays the shelf price, and `tax ≤ total`.

### E08-T02 · Discount engine and permissions
- **Size** M · **Deps** E08-T01, E05-T04 · **PRD** POS-004
- **Scope:**
  - Line and order discounts, fixed or percentage, with largest-remainder allocation.
  - Price override.
  - The cashier limit `discount.cashier_max_ppm`.
  - `APPROVAL_REQUIRED` above the limit.
  - A reason is required for overrides and above-limit discounts.
- **Acceptance:**
  - [ ] An order discount of 10.00 across lines of 3.33, 3.33 and 3.34 allocates exactly 10.00.
  - [ ] A discount larger than the line or order gives `PRICING_DISCOUNT_EXCEEDS`.
  - [ ] A cashier giving 15% with a 10% limit gets `APPROVAL_REQUIRED`. With a manager token, it is applied and the approver is recorded.
  - [ ] A 100% discount (free item) is allowed with approval, and the tax is 0.
  - [ ] An `cart.discount_applied` audit entry records the before and after amounts.

### E08-T03 · Pricing golden test suite
- **Size** S · **Deps** E08-T01
- **Scope:**
  - `tests/fixtures/pricing/*.json`, at least 40 scenarios covering every rule in ADR-004, including cash rounding.
  - The Rust test runner uses them.
  - The **same JSON files** feed a TS test that checks the UI formats the Rust output correctly (the UI does no maths).
- **Acceptance:**
  - [ ] The scenario list is reviewed by product/finance for the D-02 country before the end of Stage 1.
  - [ ] Any rounding change requires updating the goldens in the same PR, enforced by CODEOWNERS.

## E09 Cart and checkout UI

### E09-T01 · Cart backend
- **Size** M · **Deps** E06-T04, E08-T01 · **PRD** POS-001, -002, -003
- **Scope:**
  - Migration `0004_carts.sql`.
  - The `Cart` aggregate and `CartService` commands.
  - Every mutation persists the cart and bumps `revision` (local-only WriteSet: no outbox, audit only for clear/void/override/discount).
  - Returns `CartView` (lines plus `PricedCart`).
- **Acceptance:**
  - [ ] Add, set quantity, remove, clear (with revision) and discount each return a correctly priced view.
  - [ ] Setting a quantity of 0 is rejected (use remove).
  - [ ] A fractional quantity is rejected for `ea`.
  - [ ] `cart_clear` with a stale revision gives `CART_REVISION_MISMATCH`.
  - [ ] Only one open cart per register (unique index).

### E09-T02 · Cart recovery after restart
- **Size** S · **Deps** E09-T01 · **PRD** POS-011
- **Acceptance:**
  - [ ] Kill the app with 5 lines in the cart and restart: after login, a "Recovered cart (5 items, 23.40)" prompt offers **Resume** or **Discard** (discard needs `CartClear` and is audited as `cart.recovery_discarded`).
  - [ ] Prices are recomputed against the current catalogue on resume, and lines whose price changed are highlighted.

### E09-T03 · Checkout screen layout and cart table
- **Size** M · **Deps** E09-T01, E01-T02 · **PRD** §5, §5.1
- **Acceptance:**
  - [ ] The layout keeps the cart table (name / qty / unit price / line total, visually distinct), the totals panel and the action bar visible without scrolling at 1366×768.
  - [ ] The total due is at least 48 px and has a contrast ratio of 7:1 or more.
  - [ ] Quantity change, remove and discount are all done inline. The checkout screen is never left.
  - [ ] There are no animations on state changes. Line add/update has a non-animated highlight of at most 150 ms.
  - [ ] Destructive actions (clear, void) show a confirmation dialog.

### E09-T04 · Product search panel and quick-select grid
- **Size** M · **Deps** E06-T04, E09-T03 · **PRD** POS-002
- **Acceptance:**
  - [ ] Search as you type (debounce 80 ms) with keyboard navigation (↑/↓/Enter adds).
  - [ ] Search shows the price, the variant label and on-hand stock (once Stage 2 lands).
  - [ ] The quick-select grid shows products flagged `quick_select`, paged, with categories as tabs. It is touch-friendly (targets of at least 44 px).
  - [ ] Choosing a variant product opens a variant picker.

### E09-T05 · Keyboard map and accessibility
- **Size** S · **Deps** E09-T03, E09-T04 · **PRD** §5.1, §14 (Accessibility)
- **Scope:** A documented key map:
  - `F2` search
  - `F3` quantity
  - `F4` discount
  - `Del` remove line
  - `F9` pay
  - `F12` lock
  - `Esc` back
  - `+`/`-` quantity
  - `Ctrl+Shift+D` dev panel (debug only)
- **Acceptance:**
  - [ ] An e2e test completes "scan 3 items, change qty, discount, open pay" with the keyboard only.
  - [ ] An axe-core scan of the checkout screen has no serious or critical issues.
  - [ ] The UI scales to 150% text without layout breakage at 1920×1080.

### E09-T06 · Status bar
- **Size** S · **Deps** E09-T03 · **PRD** §5.1 (Offline / Sync pending indicator)
- **Acceptance:**
  - [ ] Shows the current user and role, the register code, SANDBOX/LIVE, scanner status, and a sync indicator ("Not linked" in Stage 1, wired to `sync://status` in Stage 3).
  - [ ] The indicator is persistent, never modal, and never blocks input.

### E09-T07 · Performance instrumentation
- **Size** S · **Deps** E07-T02, E09-T03 · **PRD** NFR-add-to-cart
- **Acceptance:**
  - [ ] A `performance.mark` runs from the scan burst terminator to the cart row rendered. A debug overlay shows p50 and p95.
  - [ ] An e2e perf test of 200 scans with `shop5k` gives p95 < 100 ms on the CI Windows runner (with a soft-fail budget of 150 ms on CI, and the hard target on the reference machine in E19-T01).
