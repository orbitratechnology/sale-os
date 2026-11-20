# 03 — Engineering Decisions (ADRs) and Open Questions

This document has two parts:

1. **ADRs**: engineering decisions the backlog assumes. They are recorded here so tasks can
   cite them. Changing one after Stage 0 has a cost, which is listed with each ADR.
2. **Open product decisions**: the PRD §19 questions, each with a recommended default, the
   tasks it blocks, and the date by which it must be decided.

Each ADR becomes a file under `docs/adr/` in task `E00-T03`. Here they are kept short.

---

## Part 1 — ADRs

### ADR-001 Money representation

- Money is an **`i64` count of minor units** (cents, fils, etc.). ISO-4217 currency
  metadata comes from the business: the code and the exponent (0–4).
- **Each business has exactly one currency in the MVP.** Domain `Money` values therefore do
  not carry a currency. The currency is fixed on the business, and each sale snapshots its
  `currency_code` and `currency_exponent`. If multi-currency is added later, the type becomes
  `Money { minor, currency }`.
- Intermediate calculations, such as tax on an inclusive price or percentage discounts, use
  `rust_decimal::Decimal`. They are rounded back to minor units **once**, at the points named
  in ADR-004.
- On the TypeScript side, money is a decimal **string of minor units** (e.g. `"1250"`) in
  IPC payloads. The UI only formats it. It never does arithmetic on money for business
  purposes.
- *Cost of change:* very high after Stage 1.

### ADR-002 Quantity representation

- A quantity is an **`i64` count of milli-units**: a fixed scale of 3, so 1.250 kg is `1250`.
- Whether a unit allows fractions is a property of the unit (`units.allows_fraction`). For
  example, `ea` allows integers only, so the value must be a multiple of 1000.
- Quantities are also decimal strings in IPC.
- *Cost of change:* high. If a pilot needs finer precision (for example grams of gold), add a
  new unit with a scale override instead of changing the global scale.

### ADR-003 Rates

- Tax rates and percentage discounts are **parts per million (ppm)**, stored as `u32` or
  `INTEGER`. For example, 7.5% is `75_000`, 16% is `160_000` and 100% is `1_000_000`.

### ADR-004 Rounding policy (the single documented policy, PRD §9.3)

- **Rounding mode:** half away from zero ("commercial rounding").
- **Line level:**
  - `gross = unit_price × quantity`, rounded to minor units.
  - Line discounts are applied to the rounded gross.
  - Order discounts are **allocated to lines pro rata by line net-before-order-discount**,
    using the **largest-remainder method**. The allocation always sums exactly to the order
    discount.
  - **Tax is calculated and rounded per line, per tax rate.**
- **Document level:** totals are sums of rounded line values. The document level never
  re-rounds.
- **Tax-inclusive prices:** `tax = round(net_incl × rate / (1 + rate))`, and
  `net_excl = net_incl − tax`. The customer-facing line total equals the shelf price × qty
  minus discounts.
- **Cash rounding** (for example to 0.05) is optional per business. It applies only when
  the tender is entirely cash. It is recorded as a separate `rounding_minor` on the sale, not
  folded into lines.
- **Partial returns** refund `round(line_total × cumulative_returned_qty / line_qty)` minus
  what was already refunded for that line. This makes a full return refund exactly the line
  total.
- If the D-02 pilot country requires per-invoice tax rounding, implement it as a second
  `TaxRoundingMode` variant. Do not change this default.

### ADR-005 Identifiers

- **UUIDv7**, generated on the client (time-ordered, which helps index locality).
- Stored as canonical lowercase `TEXT` in SQLite and as native `uuid` in PostgreSQL.
- Each ID kind has a Rust newtype (`SaleId`, `VariantId`, …). Mixing them up is a compile
  error.
- Human-readable numbers (receipt and return numbers) are separate. They are allocated per
  register from `register_counters` (see [04](04-local-database.md)).

### ADR-006 Time

- All instants are stored as an `INTEGER` count of **UTC epoch milliseconds**.
- Business-dated records (sales, returns, inventory documents, cash sessions) also store
  `business_date` (`YYYY-MM-DD`). It is computed at commit time from the business time zone
  (`chrono-tz`) and **never recomputed**.
- `Clock` is a port. Tests inject a fixed or stepping clock, and the timezone and day-boundary
  tests in PRD §16.3 use it.
- Device clock drift: the server records `received_at` and its own clock. Reports use the
  device's `business_date`. If a device clock differs from server time by more than 10
  minutes, the server raises a **clock-skew warning** on the sync status.

### ADR-007 SQLite durability settings (PRD §9.1, §14.1)

| Setting | Value | Why |
|---|---|---|
| `journal_mode` | `WAL` | Reads (search, reports) do not block the writer |
| `synchronous` | `FULL` | In WAL mode, `NORMAL` can lose the last commits on power loss. A completed sale must survive. |
| `foreign_keys` | `ON` | Integrity |
| `busy_timeout` | `5000` ms | Avoids spurious `SQLITE_BUSY` |
| Write transactions | `BEGIN IMMEDIATE` | Takes the write lock at the start. No lock upgrades, no deadlocks. |
| Writer concurrency | One writer connection (a serialized write actor) plus N reader connections | |
| Checkpoint | `wal_autocheckpoint` default, plus `PRAGMA wal_checkpoint(TRUNCATE)` when idle for 60 s and on clean shutdown | |
| Backups | `VACUUM INTO` before every migration, hourly when there are changes (keep 24), daily (keep 14), and on clean shutdown | |
| Integrity | `PRAGMA quick_check` on start-up. Full `integrity_check` in the diagnostics bundle. | |
| Database location | Per-user application data directory. **Never a network share** (start-up refuses UNC paths). | |

These settings are confirmed by the crash harness in task `E02-T04`, and again by the
campaign in `E19-T02`.

### ADR-008 Single write path: `WriteSet` → `LocalStore::apply`

- Every business mutation works the same way. The use case loads state through query ports,
  calls pure domain functions, and builds a **`WriteSet`**. It then calls
  `LocalStore::apply(write_set)`, which runs **one `BEGIN IMMEDIATE` transaction**.
- Validation in `WriteSet::build()` rejects any set that has no audit event, or that writes a
  syncable record without a matching outbox event.
- `apply` allocates the outbox `local_sequence` and any receipt or return numbers **inside**
  the transaction.
- Stock balance projections are updated in the same transaction as their movements.
- The UI reports success only after `apply` returns `Ok`. This covers PRD §11.2 and AT-04.

### ADR-009 Outbox from day zero

- Every syncable mutation writes an outbox event, **even when the install has never been
  linked to the cloud**.
- Linking later simply drains the backlog. There is no separate "initial upload" code path.
- Draft carts, print jobs, login throttling and UI preferences are **local only**. They never
  emit events.

### ADR-010 Sync delivery semantics

- Delivery is **at-least-once**. The server deduplicates on `(business_id, event_id)` in the
  `event_inbox` table.
- The server applies each register's events **in `local_sequence` order**, one transaction per
  event. Each transaction both applies the effect and records the inbox result (PRD §11.3).
- Each event gets a per-event result:
  - `applied`
  - `duplicate` (the original result is returned)
  - `conflict` (master data whose `base_version` does not match)
  - `rejected` (a permanent validation failure)
- `conflict` and `rejected` still count as **handled**. They are acknowledged so later events
  are not blocked, and they are surfaced on the client: conflicts go to the conflict queue
  (SYN-010) and rejections go to the repair queue (PRD §11.3).
- Transient failures (5xx, timeouts, a lock timeout) stop the batch. The client retries with
  exponential backoff and full jitter, starting at 2 s and capped at 5 min.
- `ack_sequence` is the highest **contiguous** handled `local_sequence` for the register.
- If an event arrives with a gap (sequence N+2 before N+1), the server answers
  `out_of_order` and the client resends from `ack_sequence + 1`.

### ADR-011 Master-data versioning (optimistic local versions)

- Each mutable aggregate has a `version` column: business profile, location, register,
  settings, staff user, category, tax rate, tax category, and the product aggregate (product +
  variants + barcodes).
- A local edit sets `version = version + 1` and emits an event carrying
  `base_version = old version`.
- The server accepts the edit if `server.version == base_version`, and stores
  `version = base_version + 1`. Because one register's events are applied in order, the local
  and server versions stay in step.
- If they do not match, the server answers `conflict` and returns its current snapshot. The
  client records a `sync_conflicts` row and marks the local aggregate `sync_status = conflict`.
  Later local edits to that aggregate are allowed but are also conflicted, until the conflict
  is resolved.
- Resolving a conflict emits a new event with `base_version = server_version`.
- **Facts are never versioned.** Sales, returns, refunds, inventory documents, stock
  movements, cash sessions and audit events are immutable. "Last write wins" is never applied
  to them (SYN-009).

### ADR-012 Typed IPC

- Tauri commands are thin adapters. Each one does three things: deserialize the input, call an
  application service, and map the error.
- Bindings are generated with **`tauri-specta`** into `apps/desktop-ui/src/ipc/bindings.ts`.
  CI fails if the generated file differs from what is committed.
- Every command returns `Result<T, IpcError>`. The `IpcError` shape is defined in
  [07 §2](07-contracts.md#2-error-model).

### ADR-013 Authentication and credentials

- **Staff PIN:** 4–8 digits, hashed with Argon2id (`m=64 MiB, t=3, p=1`) with a per-user salt. The originally proposed `m=19 MiB, t=2` measured only about 18 ms per verification on current hardware (E05-T02), below the 50 ms floor, so the cost was raised: about 135 ms measured. Stored hashes record their parameters, so the cost can be raised again later without breaking existing PINs. A 4–8 digit PIN cannot resist an offline attack on a stolen database whatever the cost; database encryption is evaluated in E19-T05.
  After 5 failures the account is locked for 30 s. The lockout doubles on each further failure,
  up to 15 min. Throttle state is device-local.
- **Staff PIN hashes sync** to the other registers of the same business, so a cashier can sign
  in on any register while it is offline. This is accepted because the hashes are Argon2id.
  Revisit if the D-02 compliance profile objects.
- **Cloud owner account:** email and password (Argon2id), behind an `IdentityProvider` trait so
  a managed OIDC provider can replace it (see D-09).
- **Device credentials:** a per-register refresh token (opaque, 256-bit, stored hashed on the
  server) and a short-lived access JWT (EdDSA, 15 min) carrying `business_id`, `register_id`
  and `device_id` claims. Refresh tokens rotate on every use, and reusing an old one revokes
  the device.
- The refresh token is stored in the **OS credential store** (Windows Credential Manager, via
  the `keyring` crate). It is never stored in SQLite, config files or the WebView.

- *As built (E16-T03):* JWTs are signed with Ed25519 through `ed25519-dalek` directly (no
  JWT library: `jsonwebtoken`'s pure-Rust backend pulls in `rsa`, which has an open RustSec
  advisory). Verification accepts only `alg: EdDSA`, checks the issuer, the token kind
  (`user` / `device`) and the expiry.

### ADR-014 Receipt rendering

- There is one canonical `ReceiptDocument`: a JSON-serializable model built from a sale or
  return snapshot.
- Renderers:
  - **HTML preview:** a React component that reads the same JSON.
  - **PDF:** rendered in Rust with an **embedded Noto Sans** font family, so glyph
    coverage does not depend on the OS. *Implemented (E13-T04) with `krilla` instead of
    `printpdf`*: it subsets fonts and writes ToUnicode maps (small, searchable PDFs), and
    each run is shaped with `rustybuzz`, so Arabic joins and Devanagari forms conjuncts.
    Fallback fonts cover Arabic, Devanagari, Chinese/Japanese (GB 2312 + JIS X 0208) and
    Korean (KS X 1001 Hangul); see `crates/pos-receipts/fonts/README.md`.
  - **ESC/POS:** a byte encoder with a capability profile.
- The PDF and ESC/POS renderers have separate golden test suites (PRD §12.4).

### ADR-015 Local search

- Barcode and SKU lookups use exact `UNIQUE` indexes on normalized values. The target is
  p95 < 50 ms at 100k aliases.
- Name search uses **SQLite FTS5**:
  - `unicode61 remove_diacritics 2`
  - prefix indexes 2 and 3
- The FTS index is maintained by the store inside the same transaction as the catalogue write.

### ADR-016 Cloud tenancy

- `business_id` is on every tenant row. Foreign keys are composite, `(business_id, id)`.
- PostgreSQL RLS is enabled on every tenant table. Its policy reads
  `current_setting('app.business_id')`, which is set by `SET LOCAL` at the start of each
  request transaction. The value comes from the authenticated principal, never from the
  request body.
- The application database role is **not** the table owner and does **not** have
  `BYPASSRLS`. Migrations run as a separate owner role.

### ADR-017 Deployment shape

- Modular monolith. `cloud-api` is a single binary with two roles: `--role api` and
  `--role worker`. The worker handles report rollups, export generation and backup
  verification.
- No microservices in the MVP (PRD §8.4).

### ADR-018 Frontend state

- **Rust is the authority** for carts, pricing, permissions and every persisted state.
- React uses:
  - **TanStack Query** for IPC reads, invalidated by Rust-emitted events
    (`cart://changed`, `sync://status`, …)
  - **Zustand** only for ephemeral UI state (focused panel, open dialog)
- The UI never computes totals. It displays the `PricedCart` that Rust returns.

### ADR-019 Product aggregate always has at least one variant

- The **variant is the sellable unit**. A "simple" product has exactly one default variant,
  with `is_default = 1`.
- Barcodes, prices, costs, stock and sale lines all reference `variant_id`. This removes the
  "product or variant?" branch from every query.

### ADR-020 Stock buckets

- Stock balances are kept per `(location, variant, bucket)`, where `bucket` is `sellable` or
  `damaged`.
- A return with the "damaged/non-sellable" disposition increases the `damaged` bucket
  (INV-009). Write-offs move stock out of `damaged`.
- Low-stock alerts and the "on hand" figure in checkout use `sellable` only.

---

## Part 2 — Open product decisions (PRD §19) with recommended defaults

Work proceeds on the **recommended default** until product decides otherwise. "Needed by"
is the latest point at which a different answer stops being cheap.

| ID | Question | Recommended default | Blocks | Needed by |
|---|---|---|---|---|
| D-01 | Product name and branding | Keep the codename `sale-os` in code. Product name only in `tauri.conf.json`, installer, and the UI title. | E19-T06 (installer signing identity) | Start of Stage 4 |
| D-02 | Pilot country and its tax/invoice compliance profile | **Sri Lanka (owner, 2026-11-20). Profile drafted below, to be confirmed by the owner and the shop's accountant.** Build a generic VAT/GST engine (ADR-004), tax-inclusive or tax-exclusive, multiple rates per category, no compounding. Fiscal/e-invoicing integration is out of scope until the country is known. | E08-T01 rounding variant; receipt mandatory fields (E13-T02); retention (E19-T10) | **End of Stage 1** |
| D-03 | One or several concurrent registers per location for the pilot | Support several in the data model and sync. The pilot runs **one register per location**, and the multi-register offline stock caveat (PRD §11.5) is documented in the UI. | E17 test matrix scope | Start of Stage 3 |
| D-04 | Cloud account before the first sale, or linked later? | **Linked later.** Local-first setup works with no account (ADR-009). | E04-T02 wizard flow | Start of Stage 1 |
| D-05 | Supported Windows versions and minimum hardware | Windows 11 23H2+ x64. Reference machine: 4-core CPU (≈ i3-12100 class), 8 GB RAM, SSD, 1366×768 minimum display. | E19-T01 benchmarks | Start of Stage 4 |
| D-06 | Cloud region, backup retention, budget | A single region close to the pilot. Managed PostgreSQL with point-in-time recovery for 14 days, plus daily logical backups for 35 days. | E16-T07 | Start of Stage 3 |
| D-07 | First scanner and printer models | One generic USB HID scanner, plus Epson TM-T20III (USB) and TM-m30III (Ethernet) as reference ESC/POS printers | E20 | Start of Stage 5 |
| D-08 | Real payment provider for the pilot? | **No.** Cash plus manual external tender is enough for the pilot. | PAY-007 timing | Start of Stage 4 |
| D-09 | *(new)* Build or buy the cloud owner identity | Minimal in-house email + password behind the `IdentityProvider` trait for the pilot. Move to a managed OIDC provider before general availability (needed for MFA, IAM-006). | E16-T03 | Start of Stage 3 |
| D-10 | *(new)* Can owners join an **existing** cloud business from a register that already has local data? | **No, not in the MVP.** Linking either creates a new cloud business from the local workspace, or joins an existing one from an empty local database (bootstrap). Merging two populated workspaces is post-MVP. | E17-T08 | Start of Stage 3 |
| D-11 | *(new)* Are cash sessions (open float / count / close) in the MVP? | **Yes**, minimal version (DER-001). They are needed for the "reconciled operating day" exit gate. | E15-T02 | Start of Stage 2 |
| D-12 | *(new)* Must the pilot reference PC run (acceptance, `pos-bench --scale full`, UI latency, Narrator and high contrast) pass before release? | **Decided 2026-11-20 by the owner: skipped.** The evidence is the Windows CI runner instead: the MVP acceptance suite 13/13 (`MVP acceptance suite (windows-latest)`), the Windows Rust and browser tests, and the harness baselines (`docs/perf/2026-11-17.md`, `2026-11-19-encryption.md`, `2026-11-20-ui-latency.md`). Not measured as a result: PRD §14 on the D-05 reference machine, WebView2 cold start, memory and on-screen latency on real hardware, and a manual screen-reader and high-contrast pass. The tools stay ready (pilot runbook §11) for whenever a shop PC is available. | — | Decided |

### D-02 draft: Sri Lanka compliance profile (2026-11-20, to be confirmed)

The owner chose Sri Lanka for the pilot. This draft reads the primary sources:

- Gazette Extraordinary No. 2481/22 of 27 March 2026, the tax invoice specification under
  section 20 of the VAT Act No. 14 of 2002;
- IRD Circular SEC/2026/E/03 of 20 May 2026.

Figures that come only from secondary sources are marked *(secondary)*. Have the shop's
accountant confirm them before anything is built.

**Business and VAT**

- Country LK, currency LKR (2 decimals), time zone Asia/Colombo. All three are already in the
  tables.
- VAT standard rate 18% *(secondary)*. Registration is required above LKR 60 million of
  taxable supplies a year, or LKR 15 million a quarter *(secondary)*. Many small shops are
  under it and charge no VAT.
  - **A non-registered shop must be able to run with no VAT at all:** no VAT lines on
    receipts, and no "Tax Invoice".
- **SSCL is not a tax line on sales.** The Social Security Contribution Levy is 2.5% of the
  shop's liable turnover, and liable turnover for retail is 50% of turnover *(secondary)*.
  - It is the business's own levy, not something added to each receipt.
  - The IRD invoice specification lists only the value of supply excluding VAT, the VAT
    charged and the total including VAT; SSCL does not appear in it.
  - So the tax engine needs **no compound (tax-on-tax) step**. A shop that passes SSCL on
    does it through its shelf prices.
  - An SSCL estimate (50% × 2.5% of turnover) can be an optional line in reports. The
    accountant files the return.

**Receipts and tax invoices**

- An ordinary till receipt stays as it is. The circular separates a tax invoice from
  "quotations, proforma invoices, or receipts".
- A **"TAX INVOICE"** is issued when a VAT-registered shop sells to a VAT-registered buyer, or
  when one is asked for. It must have, in force since 1 July 2026:
  - the title "TAX INVOICE", prominent;
  - **supplier:** TIN (9 digits), registered name and address, as on the VAT certificate;
    telephone optional;
  - **purchaser** (when VAT-registered): TIN, name and address;
  - **serial number `YYMMM_QQQQ_XXXXX`:**
    - `YY` is the year, `MMM` the month in capitals (`JUL`);
    - `QQQQ` is 1–15 letters or digits (for example the register code);
    - `XXXXX` is digits only, continuing from the last number issued; it may restart each
      month or year;
    - no spaces, at most 40 characters;
    - a till number such as `26NOV_R01_00042` fits;
  - **invoice date and date of supply**, both written MM/DD/YYYY;
  - **each line:** a description specific enough to identify the goods, quantity with its
    unit, unit price, and amount excluding VAT;
  - **totals in LKR to 2 decimals:** total value of supply (excluding VAT), VAT amount
    ("Total Value of Supply @ VAT Rate"), and total including VAT;
  - **VAT-taxable supplies only.** Exempt or out-of-scope items are left off, unless they are
    an integral part of a taxable supply, in which case they are shown separately;
  - **optional:** mode of payment, place of supply, total in words, logo and notes.
- **Rounding.** The specification computes VAT on the invoice total ("Total Value of Supply
  @ VAT Rate"), while ADR-004 rounds per line. With prices including VAT the two can differ
  by a cent.
  - Proposal: add the `TaxRoundingMode::PerDocument` variant that ADR-004 already reserves,
    and use it for the Sri Lanka profile.
  - The accountant should confirm the IRD requires this rather than accepting line rounding.

**Retention**

- Tax invoices, duplicates and all records are kept for **five years from the end of the
  taxable period** (VAT Act, and the record-keeping rules referred to by Gazette 2463/05).
  The E19-T10 retention default becomes 5 years after the period ends.

**E-invoicing**

- IRD is rolling out e-invoicing through a RAMIS Web API *(secondary)*:
  - a pilot is under way;
  - all VAT-registered persons are targeted from late 2026;
  - retail B2C comes last, when POS systems are linked for real-time reporting.
- Out of scope for the pilot. Watch the IRD notices.
- Firms integrating through the API before the end of 2026 may leave out the `YYMMM_QQQQ_`
  part of the serial number.

**Work this implies**

1. **A "no VAT" business:** done before this draft. The setup wizard accepts no tax rates
   and receipts then show no tax. The wizard now says so, and Sri Lanka's locale guesses
   LKR.
2. **Tax invoices:** built 2026-11-20 (status: "Sri Lanka tax invoice").
   - The supplier TIN is the business's tax registration no. (Settings > Business).
   - In the sale detail, "Issue tax invoice" asks for the buyer's TIN, name and address.
   - Each register numbers its own invoices `YYMMM_<register code>_XXXXX`, beside the
     receipt number. The sequence is never restarted.
   - It prints, saves as a PDF at receipt width or A4, and is stored immutably with a
     golden of its layout.
   - One per sale.
   - Local to the till for now.
3. **Per-document VAT rounding** (`TaxRoundingMode::PerDocument`) for Sri Lanka: **not built,
   waiting for the accountant.**
   - Today the invoice states the VAT the sale recorded (rounded per line, ADR-004), which can
     differ by a cent from "value of supply × rate".
   - If the IRD requires the latter, add the variant and use it for the `lk` profile, with
     scenarios in the E08-T03 tax suite.
4. **Retention:** `docs/policies/data-retention.md` now states the five years and what they
   mean for a till and for the cloud. Nothing is deleted on the till.
5. **Not built:**
   - an SSCL estimate line in the reports (optional);
   - credit and debit notes for a return against an invoiced sale;
   - syncing tax invoices to the cloud;
   - e-invoicing (RAMIS).

