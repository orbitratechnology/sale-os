# Stage 2 — Sales, Stock and Receipts

**Goal.** Complete the local operating loop:

- open a cash session
- sell with cash, split or external tenders
- manage stock
- process returns
- preview, export and virtually print receipts
- report
- close the session and reconcile

**Exit gate (PRD §17).** A complete local operating day can be run and reconciled on one
desktop. This is proven by the scripted e2e scenario `E15-T03`. Its expected values come
from a fixed set of hand-calculated sales.

**Rough size.** ≈ 85–105 engineer-days.

**Decision needed at the start:** D-11 (cash sessions).

---

## E10 Payments and tenders

### E10-T01 · Tender domain and settlement
- **Size** M · **Deps** E08-T01 · **PRD** PAY-001..005, POS-006, POS-007
- **Scope:**
  - `settle()` per [06 §5](../06-rust-interfaces.md#5-pos-domainsettlement-e10-t01).
  - The tender status model with `tender_status_events`.
  - The `confirmation_source` operator/provider distinction.
  - A reference-field length limit (64) and a **deny-list check** that rejects references that look like a PAN: Luhn-valid runs of 13–19 digits. This enforces PAY-005.
- **Acceptance:**
  - [ ] Cash 20.00 on a 13.45 total gives 6.55 change.
  - [ ] Split: card_external 10.00 + cash 5.00 on 13.45 gives 1.55 change, from cash only.
  - [ ] card_external 15.00 on 13.45 gives `TENDER_OVERPAY_NON_CASH`.
  - [ ] Cash 10.00 on 13.45 gives `TENDER_INSUFFICIENT { remaining: "345" }`.
  - [ ] An external reference `4111111111111111` gives `VALIDATION_FAILED` (`looks_like_card_number`), and the value is **not logged**.
  - [ ] External tenders are always stored with `status = confirmed, confirmation_source = operator`. The domain has no code path to `provider` without a `PaymentPort` result (type-level: `ConfirmationSource::Provider` is only constructible from `PaymentStatus::Approved`).
  - [ ] Cash rounding of 0.05 applies only when all tenders are cash, and is recorded in `rounding_minor`.

### E10-T02 · Mock payment provider
- **Size** M · **Deps** E10-T01 · **PRD** PAY-006, §12.5, §16.3
- **Scope:**
  - `MockPaymentProvider` implementing `PaymentPort` with the scenarios Approve, Decline, Timeout, DuplicateCallback, Delayed(d), Reverse and Unknown.
  - A `ProviderPaymentFlow` in the application layer that is **not exposed in production UI in the MVP**. It exists so the state machine is built and tested before PAY-007.
  - Idempotency key = `tender_id`.
- **Acceptance:**
  - [ ] Each scenario has a test asserting the resulting tender status:
    - Approve → confirmed/provider
    - Decline → declined, and the sale cannot complete with that tender
    - Timeout / Unknown → `reconciliation_required`, the sale is blocked, and the operator must resolve it
    - DuplicateCallback → a single status transition
    - Reverse after approve → reversed, with an audit entry
  - [ ] No test uses real card data. The fixtures use provider-style test tokens only.

### E10-T03 · Payment panel UI
- **Size** M · **Deps** E10-T01, E09-T03 · **PRD** POS-006, POS-007, PAY-003, §7.3, AT-13
- **Acceptance:**
  - [ ] A cash keypad with quick-amount buttons (exact, next round amounts, configurable notes). Change due is shown large.
  - [ ] Split tender: tenders are added in sequence, the remaining due updates after each, and a tender can be removed before completion.
  - [ ] An external tender requires choosing the method and has an optional reference. The tender row shows the label **"Externally confirmed by operator"**. The words "Approved" and "Authorized" are never used.
  - [ ] The complete button is enabled only when the remaining due is 0. Keyboard: `F9` opens, `Enter` completes the cash-exact flow.

### E09-T08 · Complete sale command (atomic local commit)
- **Size** L → split into **T08a** (domain snapshot + WriteSet, M) and **T08b** (store apply for sale tables + receipt number + tests, M)
- **Deps:** E10-T01, E11-T01, E13-T02, E02-T03 · **PRD** POS-008, -009, -010, §7.3, §11.2, AT-04, AT-05
- **Scope:**
  - Migration `0006_sales_tenders.sql`.
  - `CheckoutService::complete_sale`, exactly per the flow in [02 §4](../02-architecture-and-modules.md#4-command-flow-completing-a-cash-sale).
  - Receipt number `{prefix}{register_code}-{receipt_seq:06}`, allocated inside the transaction.
  - The `sale.completed` v1 event payload.
  - The negative-stock policy check.
  - The cart is closed in the same transaction.
- **Acceptance:**
  - [ ] **AT-04:** after completion, the sale, lines, tenders, movements, balance updates, audit, outbox and receipt rows are all present in one transaction.
    - With `FailAt(n)` injected for every statement index, there is **no** partial state.
    - The UI never shows "completed" when `apply` fails. The e2e test injects a failure and asserts the error dialog, and that the cart is intact.
  - [ ] **AT-05:** kill the process right after `checkout_complete` returns and restart. The sale and the pending outbox event are present.
  - [ ] Works with the network disabled (POS-008).
  - [ ] Receipt numbers are unique and gapless per register over 1,000 sales, including after a forced kill during commit. A gap is allowed only if a transaction rolled back, which is not possible with in-transaction allocation; the test asserts no gaps.
  - [ ] Snapshot immutability: renaming the product and changing its price and tax afterwards leaves the sale detail and receipt unchanged (POS-010).
  - [ ] Committing from a stale cart revision gives `CART_REVISION_MISMATCH`, and nothing is written.
  - [ ] Commit p95 < 150 ms for a 30-line sale on CI (the budget against NFR-commit < 500 ms).
  - [ ] The UI shows the completed state "Saved on this device · Sync pending" (or "· Not linked").

## E11 Inventory

### E11-T01 · Movement ledger and balance projection
- **Size** M · **Deps** E06-T01 · **PRD** INV-001..004, ADR-020
- **Scope:**
  - Migration `0005_inventory.sql`.
  - `LocalStore::apply` handles `append_movements` by inserting movements and upserting `stock_balances` in the same transaction.
  - Append-only triggers.
- **Acceptance:**
  - [ ] An `UPDATE` or `DELETE` on `stock_movements` raises (SQL test) (INV-003).
  - [ ] Property test: after random sequences of sales, receipts, adjustments and returns, every balance equals Σ deltas.
  - [ ] Non-tracked variants (service/non_stock) produce no movements.

### E11-T02 · Opening balances and stock receiving
- **Size** M · **Deps** E11-T01 · **PRD** INV-005, §7.4
- **Scope:**
  - `inventory_post_document` for `opening` and `receipt`: lines of variant, quantity and optional unit cost; header with supplier name (free text), reference and note.
  - The UI is a "Receive Stock" screen that supports scan-to-add lines.
  - The opening-stock CSV import reuses the E06-T07 pipeline with `kind = opening_stock`.
- **Acceptance:**
  - [ ] A receipt of 3 lines creates 3 `receipt` movements, updates the balances, writes 1 audit entry and 1 `inventory.document_posted` outbox event, all atomically (§7.4).
  - [ ] Document number `R01-IN-000001` increments per register.
  - [ ] Scanning a received item twice increments the line quantity.
  - [ ] A quantity ≤ 0 is rejected.

### E11-T03 · Stock adjustments
- **Size** S · **Deps** E11-T01 · **PRD** INV-002, INV-006
- **Acceptance:**
  - [ ] Adjustments require a reason (`damage`, `loss`, `theft`, `count_correction`, `expired` or `other`, with a note required for `other`) and record the acting user.
  - [ ] Needs `InventoryAdjust`. A cashier calling it directly gets `PERMISSION_DENIED`.
  - [ ] `write_off` moves quantity out of the `damaged` bucket.
  - [ ] A correction is a new compensating movement. Editing an old document is impossible (no command exists).

### E11-T04 · Negative stock policy
- **Size** S · **Deps** E11-T01, E09-T08 · **PRD** INV-008, §11.5
- **Acceptance:**
  - [ ] With `inventory.allow_negative = true` (the default), selling beyond on-hand completes, writes the `inventory.negative_flagged` audit entry, and the item shows a red "Negative stock" flag in the stock overview and product list.
  - [ ] With `false`, completion fails with `STOCK_NEGATIVE_BLOCKED` and lists the variants. The cart is intact.
  - [ ] The checkout line shows a non-blocking "Only 2 in stock" hint when quantity exceeds on-hand.

### E11-T05 · Stock overview, low stock and history UI
- **Size** M · **Deps** E11-T02 · **PRD** INV-007, REP-005
- **Acceptance:**
  - [ ] Stock overview: variant, on-hand (sellable), damaged, low-stock threshold, status (OK/Low/Negative), last movement. Filters for low and negative.
  - [ ] Stock history per variant lists movements with type, delta, source link (sale, return or document), user and time.
  - [ ] A banner states that "Stock on hand is calculated from recorded movements" (PRD §6.6).

### E11-T06 · Projection rebuild and verify tool
- **Size** S · **Deps** E11-T01 · **PRD** INV-004
- **Acceptance:**
  - [ ] `verify` reports any `(location, variant, bucket)` where the balance ≠ Σ movements. It runs at startup when `quick_check` passes (bounded to 2 s, otherwise deferred to idle) and in the diagnostics bundle.
  - [ ] `rebuild` (owner, from diagnostics) recomputes the balances in one transaction and audits it.

## E12 Returns and refunds

### E12-T01 · Return planning domain
- **Size** M · **Deps** E09-T08 · **PRD** RET-003, RET-004, ADR-004
- **Scope:** Migration `0008_returns.sql`, `plan_return()`, and the `return_preview` command.
- **Acceptance:**
  - [ ] For a line of 3 × 3.33 = 9.99, returning 1 refunds 3.33, a second return of 1 refunds 3.33, and a third refunds 3.33. The total refunded is exactly 9.99. With the cumulative rule, odd totals such as 10.00 over 3 give 3.33, 3.34 and 3.33.
  - [ ] Returning more than the unreturned quantity gives `RETURN_QTY_EXCEEDS { available }` (RET-004).
  - [ ] An order-level discount is honoured: the refund uses the line's net total after allocation.
  - [ ] Tax is refunded proportionally, using the same cumulative rule per tax line.
  - [ ] Sandbox sales cannot be returned in live mode.

### E12-T02 · Return command
- **Size** M · **Deps** E12-T01, E11-T01, E10-T01 · **PRD** RET-005..007, INV-009, §7.5, AT-10
- **Scope:**
  - One WriteSet: return header and lines, refunds, movements by disposition (restock → `return_restock` +sellable; damaged → `return_damaged` +damaged; none → no movement), audit, outbox `return.completed`, and the return receipt.
  - Precondition `ReturnQtyAvailable` is checked inside the transaction.
  - Approval per settings.
- **Acceptance:**
  - [ ] **AT-10:** the return links to the original sale and lines, the refund record is separate, and stock movements match the disposition. All three dispositions are tested.
  - [ ] Refund tenders must sum to the return total. Cash refunds reduce expected drawer cash (E15-T02).
  - [ ] Actor and approver are recorded (RET-006).
  - [ ] Two returns submitted concurrently for the last unit result in exactly one success.
  - [ ] The original sale rows are unchanged (RET-002, triggers).

### E12-T03 · Sales history, sale detail and return UI
- **Size** M · **Deps** E12-T02, E13-T03 · **PRD** §5 (Sales), §7.5
- **Acceptance:**
  - [ ] Sales list with date filter, receipt-number search, cashier and tender type. It is paginated and fast at 500k lines (< 300 ms per page).
  - [ ] Sale detail shows lines, discounts, tax, tenders with confirmation-source labels, returns already made, receipt preview, export PDF and reprint.
  - [ ] Return wizard: find by receipt number or scan the receipt barcode (the receipt prints a Code128 of the receipt number), pick lines and quantities, choose dispositions, refund method, then approval if required, then confirm, then show the return receipt.

### E12-T04 · Void draft cart
- **Size** S · **Deps** E09-T01 · **PRD** RET-001
- **Acceptance:**
  - [ ] Voiding a cart that has lines requires `CartVoid` (cashier: approval) and a reason, and is audited (`cart.voided`). An empty cart is just closed.
  - [ ] A completed sale has no void command. RET-002 is checked by asserting that the command registry has no `sale_void`/`sale_delete`.

## E13 Receipts and printing

### E13-T02 · ReceiptDocument model and builders
- **Size** M · **Deps** E09-T08 (snapshot types) · **PRD** RCP-001, ADR-014
- **Scope:**
  - `ReceiptDocument` v1: header (logo ref, business name/address/tax no., custom header lines), meta (receipt no., date/time in the business TZ, register, cashier), lines (name, variant label, qty × unit price, line total, discount lines), totals (subtotal, discounts, tax breakdown per rate, rounding, total), tenders (with "External" labels and references), change, footer lines, the barcode of the receipt number, and the copy marker (ORIGINAL/REPRINT/SANDBOX).
  - A builder for returns (refund receipt) and for cash session Z summaries.
  - Width-agnostic.
  - D-02 mandatory fields go into a `ComplianceProfile` hook.
- **Acceptance:**
  - [ ] Golden JSON tests for sale, split tender, discount, multi-rate tax, return and sandbox documents.
  - [ ] The document is built from the snapshot only. A test mutates the catalogue and rebuilds the document with an identical result.

### E13-T03 · HTML receipt preview (58/80 mm)
- **Size** S · **Deps** E13-T02 · **PRD** RCP-002, RCP-004
- **Acceptance:**
  - [ ] A React `ReceiptPreview` renders 58 mm (32 chars) and 80 mm (48 chars) layouts from the same JSON, with a monospace text-grid mode that matches ESC/POS line wrapping.
  - [ ] Visual regression snapshots (Playwright) for each golden document and width.

### E13-T04 · PDF renderer
- **Size** M · **Deps** E13-T02 · **PRD** RCP-003, AT-08
- **Acceptance:**
  - [ ] Exports receipt-width (58/80 mm continuous) and A4 invoice-style PDFs with the Noto Sans font embedded.
  - [ ] A golden test extracts the text with `pdf-extract`, matches the expected lines, and checks the page size.
  - [ ] Non-Latin text (Arabic, Devanagari, CJK sample product names) renders without tofu, checked through a glyph-coverage check (Arabic shaping limitations are noted; full RTL is a D-02 follow-up).
  - [ ] The save goes through the `dialog:save` path chosen by the user, and Rust writes the file. Audited as `receipt.exported_pdf`.
  - [ ] **AT-08:** works with no printer installed.

### E13-T05 · ESC/POS encoder (full)
- **Size** M · **Deps** E13-T01, E13-T02 · **PRD** RCP-005, §12.4
- **Scope:**
  - Text styles (bold, double height/width), alignment, code page selection (CP437, CP858, CP1252 and a configurable fallback, with transliteration for unsupported glyphs).
  - Line wrapping per `chars_per_line`.
  - Code128 barcode, optional QR, partial and full cut, drawer pulse (capability-gated), and a feed before cut.
  - A capability profile drives the output.
- **Acceptance:**
  - [ ] Golden byte files per golden document × {58, 80} × {cut, no-cut}. A `.txt` decoder is used for review diffs.
  - [ ] A missing capability (for example no QR) degrades to text, and that is asserted.
  - [ ] No PDF code is reused here (separate test suites per PRD §12.4).

### E13-T06 · Print queue, virtual printer and uncertain outcomes
- **Size** M · **Deps** E13-T05 · **PRD** RCP-006, RCP-007, §12.4, AT-09
- **Scope:**
  - Migration `0007_receipts_printing.sql` (`receipts`, `print_jobs`, `print_job_events`).
  - `PrintWorker` state machine: queued → submitted → completed / failed / unknown.
  - A timeout moves the job to `unknown`. **There is never an automatic resubmit after `submitted`.**
  - The operator resolves an unknown job with "Printed" or "Not printed — reprint". A reprint creates a **new** job with `copy_kind = reprint` and the REPRINT marker.
  - `VirtualPrinter` scenarios: accept, fail, timeout, paper-out, cover-open, disconnected.
  - Auto-print after a sale (setting).
- **Acceptance:**
  - [ ] **AT-09:** the ESC/POS bytes are captured to a file and match the golden.
  - [ ] Timeout leads to `unknown`, and the worker does not resubmit (asserted: the virtual printer received exactly 1 submission). The UI shows "Print status unknown — did it print?".
  - [ ] Paper-out leads to `failed` with an actionable message. Retry is explicit, and the retry of a *failed* (never-accepted) job is allowed and stays `original`.
  - [ ] A print failure never affects sale completion. Printing happens after commit.
  - [ ] Every state transition is in `print_job_events`. The reprint is audited.

### E13-T07 · Receipt settings
- **Size** S · **Deps** E13-T03 · **PRD** SET-003
- **Acceptance:**
  - [ ] Header and footer lines, a show-logo toggle, receipt prefix (register-level), paper width (58/80), auto-print on/off, and a live preview using a sample sale.
  - [ ] Changing the prefix affects only future receipts.

## E14 Reports (device-local)

### E14-T01 · Report definitions and summary queries
- **Size** M · **Deps** E09-T08, E12-T02 · **PRD** REP-001, REP-002, REP-003, §15
- **Scope:**
  - `docs/reporting-definitions.md` formalizes PRD §15. For example:
    - **Gross sales** = Σ `sale_lines.gross_minor`
    - **Discounts** = line + order, shown separately
    - **Tax** = Σ `tax_minor`
    - **Refunds** = Σ `refunds.amount_minor`, by return business date
    - **Net sales** = gross − discounts − (refund amounts excl. tax) …
  - Every figure is in tax-inclusive or tax-exclusive terms, stated per tax mode.
  - Queries by business date and range, excluding sandbox.
  - Tender mix splits external/operator from provider.
- **Acceptance:**
  - [ ] Fixture day of 12 sales and 2 returns with hand-calculated expected values. Every measure matches to the minor unit.
  - [ ] Day boundaries use `business_date`, not UTC. A sale at 23:30 local on a UTC+5:30 business lands on the local date.
  - [ ] 500k-line dataset: a 30-day summary is under 1 s. If not, implement the `daily_sales_summary` projection contingency ([04](../04-local-database.md)).

### E14-T02 · Top products and stock reports
- **Size** S · **Deps** E14-T01, E11-T05 · **PRD** REP-004, REP-005
- **Acceptance:**
  - [ ] Top N by quantity and by value for a range. Returns net off the quantity and value (documented).
  - [ ] Current stock and low-stock report, consistent with the stock overview.

### E14-T03 · Reports UI
- **Size** M · **Deps** E14-T01, E14-T02 · **PRD** §5 (Reports), §15
- **Acceptance:**
  - [ ] Daily sales, date range, payment mix, discounts, top products and low stock.
  - [ ] Every report shows a **scope badge**: "This device only" (Stage 2), with "Cloud — last synced HH:MM" and "Fully synchronized" added in Stage 3.
  - [ ] Totals reconcile with the sales list for the same range (cross-check test).

### E14-T04 · CSV exports
- **Size** S · **Deps** E14-T01 · **PRD** REP-006, CAT-009 (pulled forward; cheap)
- **Acceptance:**
  - [ ] Exports transactions (one row per line), tenders, returns, products (re-importable by E06-T07: a round-trip test), stock and each report.
  - [ ] RFC 4180 CSV, UTF-8 with BOM (Excel-friendly), decimals formatted with `.` and the currency exponent. No float conversion.
  - [ ] Audited as `report.exported`.

## E15 Audit, cash sessions and end of day

### E15-T01 · Audit conventions and viewer
- **Size** S · **Deps** E02-T03 · **PRD** IAM-004, §13
- **Acceptance:**
  - [ ] Every action in the [07 §5](../07-contracts.md#5-audit-actions-catalogue-e15-t01) catalogue is emitted by its command. A test asserts the command → audit-action mapping is complete.
  - [ ] The owner-only audit viewer filters by action, user and date, and opens the linked entity.

### E15-T02 · Cash sessions (DER-001)
- **Size** M · **Deps** E09-T08, E12-T02 · **PRD** §9.2 `cash_sessions`, Stage 2 exit gate
- **Scope:**
  - Migration `0010`.
  - Open with a float. Close with a counted amount. Expected = float + cash tenders − change − cash refunds. Variance.
  - The setting `cash.require_session_for_sale` (default true for live mode).
  - A cashier's close is a blind count: the variance is shown only with `CashSessionViewVariance`.
  - A Z-summary receipt.
- **Acceptance:**
  - [ ] With the setting on, a sale with no open session gives `CASH_SESSION_REQUIRED`.
  - [ ] Expected cash for the fixture day matches the hand-calculated value.
  - [ ] Only one open session per register. Closing is irreversible (trigger), and outbox events `cash_session.opened` and `cash_session.closed` are emitted.

### E15-T03 · End-of-day reconciliation scenario (Stage 2 exit)
- **Size** S · **Deps** all of Stage 2
- **Acceptance:**
  - [ ] A scripted e2e scenario, fully offline, using virtual devices only:
    1. Open the session with a 100.00 float.
    2. Make 12 sales: cash, split, external card with reference, discounts, a weighed item, a negative-stock item.
    3. Receive stock.
    4. Do 1 adjustment.
    5. Do 2 returns (restock and damaged, one refunded in cash).
    6. Reprint after an unknown print.
    7. Close the session.
  - [ ] The daily report, tender mix, Z-summary, stock balances and expected cash all equal the fixture's expected values.
  - [ ] The outbox holds the expected count of events, gapless.
  - [ ] Projection verify (E11-T06) reports no drift.
