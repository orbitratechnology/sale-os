---
title: "Universal Retail POS"
subtitle: "Product Requirements Document & Technical Architecture"
date: "5 November 2026"
---

**Document status:** Proposed baseline for engineering kickoff  
**Version:** 1.0  
**Date:** 5 November 2026  
**Product type:** Desktop-first, offline-first retail point-of-sale  
**Working name:** Universal Retail POS (name to be decided)

---

# Document control {.unnumbered}

| Item | Decision |
|---|---|
| Initial client | Desktop application |
| Initial operating model | Offline-first with cloud synchronization |
| Initial business scope | Product-selling shops and counter businesses |
| Initial store model | One business workspace and one location; multiple registered registers are supported by the data model, subject to synchronization policy |
| Primary target platform | Windows 11 for the first production pilot; keep the client portable to macOS and Linux. Windows 10 is legacy-only and must be explicitly assessed for ESU/LTSC status before support |
| Hardware dependency | None for software development, onboarding, or initial operation |
| Physical hardware validation | A later, explicitly planned validation phase |
| Restaurant/KDS scope | Excluded from the initial product |
| HR/ERP scope | Excluded; only minimal cashier identity and permissions are included |

# 1. Executive summary

Universal Retail POS is a fast, reliable desktop point-of-sale application for ordinary businesses that sell physical products over a counter. It is intended for grocery and convenience stores, general retail, clothing, electronics and accessories, hardware, stationery, beauty and cosmetics, mobile-phone shops, and small wholesale counters.

The product will share one commercial foundation: catalogue, barcode identification, pricing, checkout, payments, receipts, sales history, inventory movements, and basic reporting. It will not attempt to become a general ERP, restaurant operations suite, human-resources system, or appointment platform.

The application must remain useful when the internet is unavailable. The desktop client stores operational data locally in SQLite and commits sales locally. A background synchronization engine sends durable events to the cloud when connectivity is available. The cloud provides account-level backup, centralized data, reporting, and synchronization across registered devices.

The initial release is deliberately hardware-independent. Barcode input, receipt rendering, printing commands, payment outcomes, and network failures will have software simulators and automated tests. Real scanners, printers, drawers, scales, and payment terminals are not prerequisites for building the core product.

## Product statement

**A clean, fast retail POS that works locally first, syncs reliably to the cloud, and can grow into common hardware without making hardware a prerequisite.**

# 2. Product vision and principles

## 2.1 Vision

Give ordinary shops a dependable, straightforward checkout and inventory tool that can be installed on a desktop computer, used without continuous internet access, and expanded with compatible equipment when the business needs it.

## 2.2 Product principles

1. **Local-first checkout:** Product lookup, cart editing, pricing, cash checkout, and receipt creation must not wait for a cloud round trip.
2. **Reliable transaction boundaries:** A sale, its payment records, stock movements, audit record, and sync event must be committed atomically in the local database.
3. **Simple by default:** A small shop should be able to set up the business, add products, and make its first sale without configuring an ERP.
4. **One shared retail core:** Industry differences should be represented by product settings and configuration, not entirely separate POS applications.
5. **Hardware-optional:** Manual barcode entry and digital receipts must work without physical peripherals. Hardware support is added through adapters later.
6. **Honest status:** The interface must distinguish locally saved, cloud-synchronized, externally confirmed, pending, failed, and unknown payment or print states.
7. **Portable data:** Merchants must be able to export their business records in standard formats.
8. **Security by default:** Least privilege, per-user accountability, protected local sessions, tenant isolation, and no storage of raw card data.

# 3. Target users and businesses

## 3.1 Initial target businesses

| Business type | Common needs supported by the shared core |
|---|---|
| Grocery and convenience | Barcode lookup, quick checkout, unit quantities, stock alerts |
| General retail | Product catalogue, discounts, returns, inventory |
| Clothing and fashion | Variants such as size and colour, multiple barcodes |
| Electronics and accessories | SKU catalogue, sales history, optional serial reference as a later extension |
| Hardware and building materials | Units of measure and decimal quantities |
| Stationery and bookstores | Large catalogues, barcode and SKU lookup |
| Beauty and cosmetics | Variants, categories, barcode lookup |
| Mobile-phone shops | Product variants, accessories, optional IMEI/serial extension later |
| Small wholesale counters | Bulk quantities and configurable price lists as a later extension |

## 3.2 Primary personas

**Owner/operator** - sets up the business, product catalogue, prices, tax settings, and business reports.

**Cashier** - scans or searches for products, maintains a cart, accepts a tender, completes sales, and issues receipts.

**Manager** - handles approved returns, inventory adjustments, and day-to-day reporting. In a small shop this role may be performed by the owner.

**Bookkeeper/accountant (secondary)** - reviews sales, tax totals, payment breakdowns, exports, and transaction history; no accounting ledger is included in the MVP.

## 3.3 Key user stories

- As an owner, I can create a business profile and start using the POS without connecting any peripheral.
- As an owner, I can add a product manually or import a product list.
- As a cashier, I can find a product by barcode, SKU, or name and add it to a sale immediately.
- As a cashier, repeated scans of the same product increase the quantity on the cart line.
- As a cashier, I can complete a cash sale while the internet is disconnected.
- As an owner, I can see local sales immediately and cloud synchronization status separately.
- As a manager, I can process a return against an original sale and choose whether returned stock is sellable.
- As an owner, I can preview and export a receipt even when no printer is installed.
- As an owner, I can add basic cashier accounts without setting up HR, payroll, or attendance.

# 4. Goals, non-goals, and release boundary

## 4.1 MVP goals

- Desktop POS for supported Windows 11 releases.
- Simple first-run setup for one business and one physical location.
- Product catalogue, variants, SKU, barcode aliases, and categories.
- Local barcode lookup and fast cart operations.
- Pricing, discounts, and configurable basic tax rules.
- Cash sales and manual recording of externally completed tenders.
- Sales history, voids before completion, returns, and refunds recorded as auditable transactions.
- Quantity-based stock ledger, receiving, adjustments, and stock overview.
- Local receipt rendering and PDF export.
- Virtual scanner and virtual printer for development/testing.
- Local persistence, offline operation, durable sync outbox, and cloud synchronization.
- Minimal owner, manager, and cashier identities and permissions.
- Basic operational reports and CSV export.

## 4.2 Explicit non-goals for MVP

- Restaurant tables, kitchen routing, kitchen display systems, food modifiers, reservations, and delivery marketplace integrations.
- Salon appointments, staff commissions, payroll, attendance, leave, and HR.
- Full accounting/GL, invoicing automation for every jurisdiction, procurement workflows, supplier portals, or ERP.
- Pharmacy prescriptions, controlled-medicine workflows, and regulated batch traceability.
- Advanced CRM, loyalty points, marketing automation, memberships, and customer segmentation.
- Native acquiring-bank integrations, EMV terminal certification, or storing cardholder data.
- RFID, smart shelves, computer vision, and automated physical stock tracking.
- A dedicated local store server or advanced multi-register coordination service in the first release.
- A full web back-office or mobile POS client in the first release.

## 4.3 Scope guardrail

A feature belongs in MVP only if it is needed by a broad set of ordinary product-selling shops, can be implemented without a specialized industry operating model, and does not introduce disproportionate compliance or hardware dependencies.

# 5. Product experience and navigation

The desktop application should have five primary areas:

1. **Checkout** - the default, cashier-oriented screen.
2. **Products & Inventory** - catalogue, variants, barcodes, stock receiving, adjustments, and stock overview.
3. **Sales** - completed transactions, receipts, returns, refunds, and payment details.
4. **Reports** - daily sales, payment mix, discounts, top products, and low-stock views.
5. **Settings** - business profile, currency, taxes, receipt layout, staff access, register identity, and synchronization status.

## 5.1 UX requirements

- Checkout must be keyboard-first but fully usable with mouse and touchscreen.
- The application must not require navigation away from checkout for common actions such as quantity changes, item removal, discount entry, and payment selection.
- Product name, quantity, unit price, and line total must be visually distinct.
- The total due must be prominent and legible at typical cashier viewing distance.
- Search should support barcode, SKU, and product name.
- The interface should show a persistent but unobtrusive Offline / Sync pending indicator.
- No animation may delay interaction or hide a state change.
- Destructive and financially sensitive actions require confirmation and, where appropriate, manager authorization.
- Errors must tell the operator what happened and what action is available; never silently discard a sale, scan, print job, or sync event.

# 6. Functional requirements

Priority definitions: **P0** = required for MVP; **P1** = important but may follow the first pilot; **P2** = later extension.

## 6.1 Business setup and configuration

| ID | Priority | Requirement |
|---|---|---|
| SET-001 | P0 | Create a business workspace with business name, address, phone, logo, country, time zone, and base currency. |
| SET-002 | P0 | Configure one initial business location and one or more register identities. |
| SET-003 | P0 | Configure receipt header/footer, receipt numbering prefix, and paper width preview. |
| SET-004 | P0 | Configure basic tax rates and whether prices are tax-inclusive or tax-exclusive. |
| SET-005 | P0 | Configure default unit of measure and stock tracking per product. |
| SET-006 | P1 | Configure multiple price lists and customer-specific pricing. |
| SET-007 | P1 | Configure additional locations and transfer stock between them. |

Organization management is intentionally shallow in MVP: a workspace, a business profile, one location, and registered desktop terminals. Multi-entity corporate hierarchy and franchise management are out of scope.

## 6.2 Products and catalogue

| ID | Priority | Requirement |
|---|---|---|
| CAT-001 | P0 | Create, edit, archive, and search products. |
| CAT-002 | P0 | Store name, SKU, description, category, unit, cost, selling price, tax category, and active status. |
| CAT-003 | P0 | Support multiple barcode aliases for one sellable product or variant. |
| CAT-004 | P0 | Enforce barcode uniqueness within a business unless an explicit supported packaging/alias rule is used. |
| CAT-005 | P0 | Support optional variants such as size, colour, model, or pack size. Each variant may have its own SKU, barcode, and price. |
| CAT-006 | P0 | Support stock-tracked items and non-stock/service items. |
| CAT-007 | P0 | Support decimal quantities and units such as each, kg, g, litre, metre, and box. Scale integration is not required for MVP. |
| CAT-008 | P0 | Import products from CSV with field mapping, preview, validation, duplicate detection, and an error report. |
| CAT-009 | P1 | Export catalogue data to CSV. |
| CAT-010 | P1 | Store product images locally and synchronize them through object storage. |

Product identification must be separate from product description and price. A barcode generally carries an identifier; the store's own catalogue supplies the price, tax, stock, and display information. The barcode model should be extensible to common retail symbologies and GS1 data formats [7].

## 6.3 Barcode input and scanning

| ID | Priority | Requirement |
|---|---|---|
| BAR-001 | P0 | Provide a manual barcode/SKU input path. |
| BAR-002 | P0 | Provide a keyboard-wedge input mode that can receive scanner keystrokes and configured suffix characters such as Enter. |
| BAR-003 | P0 | Provide a virtual scanner in development and automated tests. |
| BAR-004 | P0 | Resolve a barcode to a product/variant using a local indexed lookup. |
| BAR-005 | P0 | Repeated scans increase the quantity of the matching cart line by the configured scan unit. |
| BAR-006 | P0 | Unknown barcodes produce a clear not-found state and allow search or product creation by an authorized user. |
| BAR-007 | P0 | Debounce accidental duplicate events only when they are demonstrably the same scanner event; do not discard legitimate rapid repeat scans. |
| BAR-008 | P1 | Add real HID scanner integration and a physical-device compatibility test suite. |
| BAR-009 | P2 | Add serial scanner protocols, camera decoding, GS1 AI parsing, and specialized variable-measure barcode profiles. |

## 6.4 Checkout and sales

| ID | Priority | Requirement |
|---|---|---|
| POS-001 | P0 | Create a local cart with one or more product lines. |
| POS-002 | P0 | Add products by barcode, SKU, name search, or product quick-select. |
| POS-003 | P0 | Edit quantity, remove a line, and clear a cart with confirmation. |
| POS-004 | P0 | Support line-level and sale-level fixed or percentage discounts with configured permissions. |
| POS-005 | P0 | Calculate subtotal, discount, tax, rounding, and total due using deterministic decimal-safe arithmetic. |
| POS-006 | P0 | Support cash tender, change due, and split tender records. |
| POS-007 | P0 | Allow recording an externally completed card, bank transfer, or QR tender with method and optional reference. This is a manual record only, not payment processing. |
| POS-008 | P0 | Complete a sale locally while offline when all tender requirements are satisfied. |
| POS-009 | P0 | Generate a unique sale ID and human-readable receipt number per register. |
| POS-010 | P0 | Save a complete immutable transaction snapshot including item name, SKU, price, discount, tax, and unit at time of sale. |
| POS-011 | P0 | Resume or recover an uncompleted cart after an application restart, with an explicit option to discard it. |
| POS-012 | P1 | Park and retrieve carts. |
| POS-013 | P1 | Support quotations and convert a quotation to a sale. |
| POS-014 | P2 | Support customer account credit, layaway, gift cards, and store credit. |

## 6.5 Payment records

| ID | Priority | Requirement |
|---|---|---|
| PAY-001 | P0 | Treat sale/order and payment/tender as separate domain records. |
| PAY-002 | P0 | Support cash and split tender. |
| PAY-003 | P0 | Support manual recording of externally completed payment methods with a clear "external confirmation" label. |
| PAY-004 | P0 | Distinguish pending, confirmed, declined, reversed, refunded, and reconciliation-required states where applicable. |
| PAY-005 | P0 | Never store PAN, CVV, PIN, magnetic-stripe data, or other sensitive card authentication data. |
| PAY-006 | P0 | Provide a mock payment provider for approval, decline, timeout, duplicate callback, and uncertain outcome tests. |
| PAY-007 | P1 | Add one real payment provider/acquirer connector after commercial and technical agreements are secured. |
| PAY-008 | P1 | Implement payment reconciliation for provider transaction references and settlement exports. |

Offline card processing is not included by default. An offline card payment is only possible where a specific authorized provider and terminal explicitly support it. The POS must never label a locally recorded card tender as processor-approved unless it has received the provider's authoritative confirmation.

## 6.6 Inventory

| ID | Priority | Requirement |
|---|---|---|
| INV-001 | P0 | Maintain quantity on hand by product/variant and location. |
| INV-002 | P0 | Record stock movements for opening balance, stock receipt, sale, return, damage, loss, and manual adjustment. |
| INV-003 | P0 | Keep stock movement history immutable; corrections are compensating movements, not edits to old movements. |
| INV-004 | P0 | Maintain a balance projection for fast reads while retaining the movement ledger as history. |
| INV-005 | P0 | Receive stock through a simple stock-receipt workflow; supplier management is optional and minimal. |
| INV-006 | P0 | Support stock adjustment with reason and staff identity. |
| INV-007 | P0 | Support low-stock thresholds and a low-stock view. |
| INV-008 | P0 | Support negative stock as a configurable policy; if permitted, clearly flag it. |
| INV-009 | P0 | On return, allow restock, damaged/non-sellable, or no-stock-change disposition. |
| INV-010 | P1 | Add cycle counts and stocktake reconciliation. |
| INV-011 | P2 | Add lot, expiry, serial, IMEI, and warranty tracking as optional specialist extensions. |

Stock on hand is an estimate derived from recorded movements. Barcode scanning does not prove that an item physically exists, and the POS cannot detect unrecorded theft or damage. Physical counts remain necessary.

## 6.7 Returns, voids, and refunds

| ID | Priority | Requirement |
|---|---|---|
| RET-001 | P0 | Allow voiding a draft cart before sale completion. |
| RET-002 | P0 | Do not delete or silently rewrite a completed sale. |
| RET-003 | P0 | Create a return referencing the original sale and original line. |
| RET-004 | P0 | Prevent return quantities from exceeding the unreturned quantity unless a manager-controlled exception is implemented later. |
| RET-005 | P0 | Record refund tender and refund status separately from the return. |
| RET-006 | P0 | Record who performed and, where required, who approved the return. |
| RET-007 | P0 | Create appropriate stock movement based on return disposition. |

## 6.8 Receipts and print preparation

| ID | Priority | Requirement |
|---|---|---|
| RCP-001 | P0 | Generate a canonical receipt document from a completed sale. |
| RCP-002 | P0 | Preview receipts in the application without a printer. |
| RCP-003 | P0 | Export/save receipts as PDF. |
| RCP-004 | P0 | Support configurable 58 mm and 80 mm thermal-paper layouts in preview. |
| RCP-005 | P0 | Build ESC/POS command encoding and a virtual printer that captures output to a test log/file. |
| RCP-006 | P0 | Queue print jobs and track queued, submitted, completed, failed, and unknown states. |
| RCP-007 | P0 | Prevent automatic duplicate printing after an uncertain printer response; require explicit reprint action. |
| RCP-008 | P1 | Add real Windows print spooler and raw ESC/POS network/USB adapters after device validation. |
| RCP-009 | P2 | Add Star and other vendor-specific print adapters and label printer formats. |

Receipt document content is independent of printer control. The renderer creates a canonical document; output adapters convert it to PDF, HTML, operating-system print jobs, or a printer-specific command stream. Epson and other manufacturers publish printer-specific API/command references; command support and cutting behaviour must be validated per model [8].

## 6.9 Staff and access control

| ID | Priority | Requirement |
|---|---|---|
| IAM-001 | P0 | Provide owner, manager, and cashier roles. |
| IAM-002 | P0 | Allow the owner to create, deactivate, and reset basic cashier accounts/PINs. |
| IAM-003 | P0 | Enforce permissions in the application/domain layer, not only by hiding UI controls. |
| IAM-004 | P0 | Record the acting user on sales, returns, discounts, and inventory adjustments. |
| IAM-005 | P0 | Lock the register after configurable inactivity and require re-authentication. |
| IAM-006 | P1 | Add stronger account recovery and optional MFA for cloud account access. |

No payroll, attendance, job scheduling, leave, or HR records are included.

## 6.10 Reports and exports

| ID | Priority | Requirement |
|---|---|---|
| REP-001 | P0 | Show sales totals by day and date range. |
| REP-002 | P0 | Show gross sales, discounts, tax, refunds, and net sales as separate measures. |
| REP-003 | P0 | Show payment totals by tender type. |
| REP-004 | P0 | Show top-selling products by quantity and value. |
| REP-005 | P0 | Show current stock and low-stock products. |
| REP-006 | P0 | Allow transaction, product, and basic report export to CSV. |
| REP-007 | P1 | Add cost-based gross margin reporting after cost history and valuation rules are implemented. |
| REP-008 | P1 | Add consolidated cloud reports across registered registers. |

## 6.11 Cloud account and synchronization

| ID | Priority | Requirement |
|---|---|---|
| SYN-001 | P0 | Register a desktop installation as a business register/device. |
| SYN-002 | P0 | Download the current business catalogue, settings, and permissions to the local database. |
| SYN-003 | P0 | Save local mutations and outgoing sync events in one local database transaction. |
| SYN-004 | P0 | Retry synchronization automatically with exponential backoff and jitter. |
| SYN-005 | P0 | Use globally unique event IDs and server-side idempotency to prevent duplicate application. |
| SYN-006 | P0 | Acknowledge events only after the server transaction commits. |
| SYN-007 | P0 | Show last successful sync time, pending event count, and actionable sync errors. |
| SYN-008 | P0 | Support full bootstrap and incremental pull of changed master data. |
| SYN-009 | P0 | Keep sales and stock movements append-only; do not resolve them with last-write-wins. |
| SYN-010 | P0 | Detect concurrent master-data edits using entity version/base-version checks and expose conflicts. |
| SYN-011 | P1 | Support additional locations and stronger multi-register coordination. |

# 7. Core business workflows

## 7.1 First-run setup

1. Install and launch the desktop application.
2. Create or sign in to the cloud business account when internet is available.
3. Enter business profile, country, time zone, currency, and tax defaults.
4. Create the initial location and register identity.
5. Create the owner account and optional cashier accounts.
6. Add products manually or import CSV.
7. Configure receipt layout and test PDF/virtual printing.
8. Run a demo sale in sandbox mode.
9. Enable live sales.

If the user starts without internet, local setup must still be possible. Cloud account linking and synchronization can be completed later; the application must clearly explain which capabilities remain local until linked.

## 7.2 Barcode sale

1. The cashier focuses the barcode input or leaves the scanner-ready control active.
2. A HID scanner, manual entry, or simulator supplies a barcode string.
3. The input adapter identifies the scan boundary (for example, configured Enter suffix).
4. The catalogue service performs a local indexed barcode lookup.
5. If found, the product/variant is added to the cart. A repeated scan increments quantity.
6. The pricing engine computes the line and cart totals using the current local price/tax configuration.
7. If unknown, show a not-found state with search/create options.
8. The cashier reviews the cart and selects payment.

## 7.3 Complete a cash sale while offline

1. Cart and prices are calculated locally.
2. The cashier enters the cash received amount.
3. The system calculates change and validates the amount.
4. On confirmation, a single local transaction writes the sale, lines, payment, stock movements, audit event, receipt data, and outbox event.
5. The UI displays the completed receipt and a clear "Saved on this device - Sync pending" state.
6. The local receipt can be previewed or saved as PDF; physical printing is not required.
7. When internet returns, the sync engine sends the event and marks it acknowledged only after server commit.

## 7.4 Stock receiving

1. User opens Products & Inventory > Receive Stock.
2. User selects products and quantities, with optional cost and reference.
3. User confirms the receipt.
4. The system commits a stock receipt movement and updates the local balance projection atomically.
5. An audit entry and sync event are recorded.

## 7.5 Return

1. Find the original transaction by receipt number or transaction search.
2. Select original line(s) and return quantities.
3. Select disposition: restock, damaged/non-sellable, or no stock change.
4. Select refund method and record the refund result.
5. The system commits the return, refund record, inventory movement (if appropriate), audit entry, and sync event.

# 8. Technical architecture

## 8.1 Recommended stack

| Layer | Technology | Responsibility |
|---|---|---|
| Desktop shell | Tauri 2 | Cross-platform desktop packaging and native IPC |
| UI | React + TypeScript | Checkout and management interface |
| Desktop domain/core | Rust | Authoritative local business rules, commands, sync engine, future device adapters |
| Local database | SQLite | Durable operational data and outbox |
| Cloud API | Rust + Axum | Authenticated APIs, sync ingestion, reports, business settings |
| Cloud database | PostgreSQL | Central business data and event/inbox storage |
| Data access | SQLx or an equivalent maintained Rust SQL layer | Database access and migrations |
| API contract | REST + OpenAPI | Versioned client/server contract |
| Background work | Rust async worker within the desktop client; cloud worker where needed | Sync, retries, uploads, reconciliation |
| File/object storage | S3-compatible object storage | Product images, generated exports, optional backups |
| Observability | Structured logs + error reporting + metrics | Diagnostics without exposing customer/payment secrets |

This stack deliberately keeps business rules in Rust so the same domain crate can be reused by the local desktop process and cloud service. React is responsible for presentation, not for being the sole authority for pricing, permissions, or transaction invariants.

Tauri uses a native Rust layer with HTML rendered in the operating system WebView and message passing between the frontend and Rust backend [1]. Its capability/permission model should be configured narrowly so each window can access only the native commands it needs [2].

## 8.2 High-level component diagram
```text
+---------------------------------------------------------------+
|                    Desktop POS (Tauri 2)                      |
|                                                               |
|  +----------------------+       +---------------------------+ |
|  | React / TypeScript   | <---> | Typed Tauri IPC Commands  | |
|  | Checkout + Backoffice|       +-------------+-------------+ |
|  +----------------------+                     |               |
|                                               v               |
|  +---------------------------------------------------------+  |
|  | Rust Application Core                                   |  |
|  | Catalogue | Pricing | Sales | Payments | Inventory      |  |
|  | Receipts  | Permissions | Reports | Sync                |  |
|  +---------------------------+-----------------------------+  |
|                              |                                |
|               +--------------+--------------+                 |
|               v                             v                 |
|       +---------------+              +---------------+        |
|       | SQLite        |              | Adapter ports |        |
|       | Business data |              | Virtual in MVP|        |
|       | Outbox        |              | Physical later|        |
|       +---------------+              +---------------+        |
+------------------------------|--------------------------------+
                               | HTTPS REST
                               v
+---------------------------------------------------------------+
|                         Cloud Platform                        |
|  +----------------------+       +---------------------------+ |
|  | Rust / Axum API      | ----> | PostgreSQL                | |
|  | Auth + Sync + Reports|       | Tenant data + event inbox | |
|  +----------------------+       +---------------------------+ |
|                  |                                            |
|                  v                                            |
|        Object storage / backups / telemetry                   |
+---------------------------------------------------------------+
```

## 8.3 Architectural boundaries

**Frontend:** renders UI, gathers operator input, and calls typed application commands. It must not write directly to SQLite or bypass domain validation.

**Domain core:** contains product, price, tax, cart, sale, payment, return, and inventory rules. It should not depend on Tauri UI APIs.

**Local persistence:** stores operational state, migration version, outbox, sync cursor, and local audit history.

**Sync engine:** transports events and master-data changes. It must not recalculate a completed sale using new prices or tax rules.

**Cloud API:** authenticates devices, enforces business/tenant access, accepts idempotent events, serves master-data changes, and provides consolidated reports.

**Device ports:** define stable interfaces for barcode input, receipt output, and future peripherals. MVP uses virtual adapters and manual input.

## 8.4 Suggested repository structure
```text
/apps
  /desktop-ui             # React + TypeScript
  /desktop-tauri          # Tauri entry point and IPC commands
  /cloud-api              # Axum API service
/crates
  /pos-domain             # Shared domain types and business rules
  /pos-application        # Use cases / application services
  /pos-local-db           # SQLite repositories and migrations
  /pos-cloud-db           # PostgreSQL repositories and migrations
  /pos-sync               # Event envelopes, sync protocol, conflict types
  /pos-device-ports       # Scanner/printer/payment abstractions
  /pos-device-virtual     # Simulated devices and test doubles
  /pos-receipts           # Receipt document model and renderers
/contracts
  /openapi
  /events
/tests
  /fixtures
  /integration
  /e2e
```

A modular monolith is preferred over microservices for MVP. Split services only when scale, deployment independence, or operational needs justify the additional complexity.

# 9. Local data architecture

## 9.1 SQLite responsibilities

SQLite is the local system of operation for a register. It stores:

- Business and location profile cached for the register.
- Register and staff/session information needed offline.
- Products, variants, categories, barcodes, and price/tax configuration.
- Inventory movement history and balance projections.
- Draft carts and completed sales.
- Payment/tender records and refunds.
- Receipt documents and print-job state.
- Audit events.
- Sync outbox and inbound sync cursor.

Use versioned schema migrations. Enable foreign keys. Use transactions for every operation that spans multiple domain records. WAL mode may improve reader/writer concurrency, but checkpointing, backup, and synchronous/durability settings must be selected deliberately and tested on target operating systems [4][5].

Do not place a live SQLite database on a network share as a multi-terminal shared database. Each register owns its local database and synchronizes via the API.

## 9.2 Conceptual local tables

| Table | Important fields / purpose |
|---|---|
| businesses | business_id, name, country, timezone, base_currency |
| locations | location_id, business_id, name, status |
| registers | register_id, location_id, name, device_id, status |
| staff_users | user_id, business_id, display_name, role, pin_hash, status |
| categories | category_id, business_id, name, parent_id nullable |
| products | product_id, business_id, name, type, SKU, category_id, active |
| product_variants | variant_id, product_id, options_json, SKU, price, cost |
| product_barcodes | barcode_id, business_id, variant_id, value, symbology, qualifier |
| tax_rules | tax_rule_id, business_id, name, rate, mode, effective dates |
| stock_movements | movement_id, product/variant, location, type, quantity_delta, reference |
| stock_balances | product/variant, location, on_hand, updated_at (projection) |
| sales | sale_id, business, location, register, receipt_no, status, totals, timestamps |
| sale_lines | line_id, sale_id, product snapshot, quantity, price, discount, tax, total |
| tenders | tender_id, sale_id, type, amount, status, external_reference nullable |
| returns | return_id, original_sale_id, status, totals, actor |
| return_lines | return_line_id, return_id, original_line_id, quantity, disposition |
| refunds | refund_id, return_id/sale_id, tender_type, amount, status |
| cash_sessions | session_id, register_id, user_id, opening_cash, closing_cash, status |
| receipts | receipt_id, sale_id, content_version, document_json |
| print_jobs | job_id, receipt_id, adapter, status, attempt_count |
| audit_events | audit_id, business_id, actor_id, action, entity, timestamp, metadata |
| outbox_events | event_id, register_id, local_sequence, event_type, payload, created_at, status |
| sync_cursors | register_id, last_server_cursor, last_ack_sequence |
| sync_conflicts | conflict_id, entity_type, entity_id, local_version, server_version, status |

The final physical schema should use UUIDs (or similarly collision-resistant IDs) for distributed entities and composite foreign-key/tenant constraints where appropriate.

## 9.3 Financial and quantity representation

- Do not use binary floating point for prices, taxes, discounts, or tender amounts.
- Store money as integer minor units with currency metadata, or a fixed-precision decimal representation with documented precision and rounding.
- Store quantities as fixed-precision decimals to support kg, g, litres, metres, and fractional units.
- Store sale-line price, discount, tax basis, tax rate, and description as a snapshot so historic receipts do not change when catalogue data changes.
- Record UTC timestamps plus the business time zone/local business date needed for daily reports.
- Document a single rounding policy and test it at line and invoice level.

# 10. Cloud architecture and tenancy

## 10.1 Cloud responsibilities

- Business account and register/device authorization.
- Central copy of business configuration and catalogue.
- Idempotent ingestion of sales, returns, payments, inventory movements, and audit events.
- Device sync cursors and acknowledgement state.
- Consolidated reports.
- Object storage for product images and exports.
- Backup and restore facilities.
- Device revocation and app version policy.

## 10.2 Tenant isolation

Every business-owned record must carry a business_id. Every request must derive the authenticated business scope from the server-side device/user identity, not trust a business_id supplied by the client. Use database constraints and, where appropriate, PostgreSQL Row-Level Security as a defense-in-depth layer [6]. Test cross-tenant access attempts explicitly.

## 10.3 Suggested API groups

| Method / route | Purpose |
|---|---|
| POST /v1/auth/device/register | Register a desktop installation |
| POST /v1/auth/device/refresh | Rotate/refresh device credentials |
| GET /v1/sync/bootstrap | Download initial business data |
| GET /v1/sync/changes?cursor=... | Pull incremental server changes |
| POST /v1/sync/events | Upload outbox events idempotently |
| GET /v1/sync/ack?register_id=... | Inspect acknowledged register sequence |
| GET /v1/catalog/products | Search/filter catalogue for administrative clients |
| POST /v1/catalog/products | Create or update product with expected version |
| GET /v1/reports/sales | Consolidated sales report |
| GET /v1/health | Health check |

The precise route list may change, but API contracts must be versioned and documented with OpenAPI.

# 11. Offline synchronization design

## 11.1 Source-of-operation model

- The desktop register is authoritative for its own local draft carts, completed local transactions, and local device state.
- The cloud stores the consolidated copy and provides shared master data/configuration.
- Completed sale and stock-movement events are immutable facts; they are not overwritten by a later device snapshot.
- Product, tax, and settings records are versioned mutable master data.
- Cloud synchronization is eventual, not a prerequisite for local cash checkout.

## 11.2 Transactional outbox

When a sale is completed, perform one SQLite transaction:

1. Insert the sale header.
2. Insert immutable sale lines and price/tax snapshots.
3. Insert tender records.
4. Insert stock movement records and update balance projections.
5. Insert audit event.
6. Insert a sync outbox event with a globally unique event_id and monotonically increasing register-local sequence.
7. Commit.

If any step fails, roll back the entire transaction. The UI must not tell the cashier the sale completed until the local commit succeeds.

## 11.3 Event delivery protocol

- Delivery is at-least-once; exactly-once network delivery is not assumed.
- Every event has event_id, business_id, location_id, register_id, local_sequence, event_type, schema_version, occurred_at, and payload.
- The server stores a unique inbox key for event_id (scoped to business/register as appropriate).
- The server applies the event and records its acknowledgement in the same PostgreSQL transaction.
- A repeated event_id returns the original acknowledgement without applying the effect again.
- The register advances its acknowledged sequence only after receiving server confirmation.
- Failed transient requests retry with exponential backoff and jitter; permanent validation errors are placed in a visible repair queue.
- Event schemas are versioned. Unknown mandatory event versions are rejected with an actionable upgrade/compatibility message rather than silently ignored.

## 11.4 Master-data synchronization and conflicts

- Pull remote changes using a monotonic server cursor or change sequence.
- Each mutable entity has a version.
- Local edits made offline include the version they were based on.
- If the server version still matches the base version, accept the update and increment the version.
- If it does not match, create a sync conflict; do not silently overwrite the newer server record.
- MVP conflict UI should support viewing the local and server versions and selecting a resolution with an audit event.
- The owner/manager should be able to choose the accepted name, SKU, price, tax category, or other changed fields.

## 11.5 Multi-register inventory policy

The first release may allow more than one registered register in one location, but it does not promise globally reserved stock while terminals are simultaneously offline. Each terminal can sell based on its last known stock state. The cloud aggregates immutable movements; a balance may temporarily or permanently become negative. Negative stock must be flagged and visible. Strict offline stock reservation and a local store gateway are future capabilities.

## 11.6 Offline capability matrix

| Capability | Offline behaviour |
|---|---|
| Browse cached products | Available |
| Barcode/SKU lookup | Available |
| Cart and pricing using cached rules | Available |
| Cash sales | Available |
| Manual external tender recording | Available, clearly marked as externally confirmed by operator |
| Real card authorization | Not available unless a future provider explicitly supports it |
| Local receipt preview/PDF | Available |
| Physical printing | Available only after a local printer adapter exists and device is connected |
| Inventory movement entry | Available locally |
| Cloud reports | Last synchronized data only; label as stale |
| Product edits | Available with versioned conflict handling |
| Cloud backup/sync | Queued until connectivity returns |

# 12. Hardware abstraction and simulation-first development

## 12.1 Design principle

The POS domain must not depend on a specific scanner, printer, operating system driver, or vendor SDK. Device access belongs behind ports/adapters. The initial build uses virtual adapters so application logic can be developed and tested without equipment.

## 12.2 MVP device ports
```rust
trait BarcodeInputPort {
    fn start(&self) -> Result<()>;
    fn stop(&self) -> Result<()>;
    fn subscribe(&self) -> ScanEventStream;
}

trait ReceiptOutputPort {
    fn submit(&self, job: ReceiptPrintJob) -> Result<PrintJobId>;
    fn get_status(&self, job_id: PrintJobId) -> Result<PrintStatus>;
}

trait PaymentPort {
    fn initiate(&self, request: PaymentRequest) -> Result<PaymentAttempt>;
    fn query(&self, attempt_id: PaymentAttemptId) -> Result<PaymentStatus>;
}
```

These are conceptual interfaces; the concrete Rust types should be designed around async execution, cancellation, timeouts, and structured errors.

## 12.3 Virtual scanner

The virtual scanner must support:

- Typing/pasting a barcode.
- Configurable suffix (Enter/Tab/none).
- Burst scans and inter-scan delay.
- Unknown barcode.
- Malformed/incomplete input.
- Duplicate event delivery.
- Scanner disconnect/reconnect simulation.
- Multiple symbology test fixtures.

The real first hardware mode should be USB HID keyboard wedge because many scanners can emulate keyboard input. More specialized serial/vendor SDK support comes after real equipment is acquired. Barcode data formats should be extensible; GS1 barcodes can encode identifiers and attributes such as batch/lot numbers and dates, not merely a product number [7].

## 12.4 Virtual printer

The virtual printer must:

- Capture the exact ESC/POS byte stream or structured print commands.
- Save output to a test artifact.
- Simulate accepted, failed, timed-out, paper-out, cover-open, and disconnected outcomes.
- Allow configurable printer capabilities such as paper width, cut, cash drawer pulse, and image/QR support.
- Verify that a retry does not accidentally create duplicate printed receipts.

PDF receipt rendering and ESC/POS encoding must be separate test suites. A visually correct PDF does not prove that a physical printer will render every glyph or command correctly.

## 12.5 Virtual payment provider

Simulate approved, declined, timed out, duplicate callback, delayed confirmation, reversed, and unknown payment states. Never use real card data in tests.

## 12.6 Hardware integration sequence after MVP

1. Windows USB HID barcode scanner.
2. Windows PDF/OS printer path.
3. ESC/POS USB or Ethernet receipt printer.
4. Cash drawer through compatible printer kick port.
5. Serial/USB weighing scale.
6. Vendor-specific adapters and payment terminal integrations.

Maintain a hardware compatibility registry containing vendor, model, connection type, OS, adapter version, tested capabilities, firmware, and test date.

# 13. Security, privacy, and operational safeguards

- All cloud traffic uses TLS.
- Device credentials are unique per register and revocable.
- Secrets are stored through operating-system credential facilities where possible; do not hardcode secrets in the frontend or configuration files.
- Tauri capabilities and commands follow least privilege. The WebView should not receive unrestricted file-system, shell, or native-device access [2].
- Local database access is restricted to the application/user profile. Evaluate database encryption against the threat model; do not claim that SQLite is encrypted by default.
- Local staff PINs must be securely hashed using a password/PIN hashing function with per-user salt. Apply retry limits and lockout/backoff.
- Sensitive actions (refund, discount above threshold, stock adjustment, void after completion) require permission checks and audit events.
- No raw card number, CVV, PIN block, or magnetic-stripe data is stored or logged.
- Logs must redact access tokens, PINs, and personal data not required for diagnostics.
- Backups must be encrypted in transit and at rest, access-controlled, and periodically restore-tested.
- App updates should be signed and delivered over a trusted update channel.
- Business data export must be available to the business owner.
- Define data retention and deletion behaviour before production launch.
- Obtain legal/security review for the countries in which the POS is sold; this PRD is not a legal or PCI certification.

# 14. Non-functional requirements and quality targets

Targets below are engineering goals to validate on a documented reference Windows machine and representative dataset; they are not claims about all hardware.

| Area | Target |
|---|---|
| Local barcode lookup | p95 under 50 ms on a reference machine with 50,000 active variants and indexed barcodes |
| Add item to visible cart | p95 under 100 ms after completed scan input |
| Product search | p95 under 150 ms for local catalogue queries at target dataset size |
| Cash sale local commit | p95 under 500 ms, excluding operator input and receipt printing |
| Cold application launch | Target under 5 seconds on reference hardware after first initialization |
| Offline operation | Core sales available for at least 7 consecutive days without cloud access, subject to cached credentials/configuration and local disk health |
| Sync correctness | No duplicate sale, tender, refund, or stock movement when events are delivered more than once |
| Crash recovery | No partially committed sale after forced termination at any commit boundary |
| Catalogue scale | Automated performance test at 50,000 variants and 100,000 barcode aliases; design should scale beyond this |
| Sales history | Support at least 500,000 sale lines in benchmark data without unacceptable lookup/report degradation |
| Accessibility | Keyboard operation for all core cashier workflows; readable contrast and scalable text |
| Reliability | Recover from transient network failures and device adapter failures without losing committed local data |
| Observability | Structured local logs, sync status, error codes, and privacy-safe diagnostics |

## 14.1 Data durability

Tune SQLite journal mode, synchronous settings, checkpoint strategy, and backup mechanism deliberately. SQLite WAL improves read/write concurrency but introduces WAL/checkpoint considerations; validate the chosen settings under forced application exit, OS crash, and power-loss simulation where feasible [4]. Use SQLite's transaction semantics for atomic multi-record operations [5].

## 14.2 Availability

The local checkout path must not depend on cloud service availability. Cloud availability affects synchronization, cross-register visibility, backup, and consolidated reporting, not whether a local cash sale can be committed.

# 15. Reporting definitions

Use unambiguous definitions in every report:

- **Gross sales:** sum of sale line amounts before discounts and returns, using a documented rule.
- **Discounts:** line and order discounts separately, plus total discount.
- **Tax:** tax amount captured on the sale snapshot.
- **Refunds:** amounts refunded, separate from sales.
- **Net sales:** gross sales minus discounts and returns/refunds according to the published report definition.
- **Tender totals:** sum by recorded tender type; distinguish external/manual records from provider-authorized transactions.
- **Stock on hand:** latest balance projection from recorded stock movements, not a physical count guarantee.
- **Gross margin:** only shown when cost basis and valuation policy are sufficiently reliable; do not label sales minus current cost as accounting profit.

Report screens must state whether numbers are device-local, last-synced cloud totals, or fully synchronized totals.

# 16. Testing and quality plan

## 16.1 Unit tests

- Price, tax, discount, rounding, change due, and split tender calculations.
- Barcode normalization, parsing, alias lookup, and repeated-scan rules.
- Inventory movement arithmetic and return disposition.
- Permission matrix.
- Sale and return state transitions.
- Receipt document generation and ESC/POS encoding.
- Event schema validation and version handling.

## 16.2 Database and integration tests

- Atomic completion of sale + tender + stock movements + audit + outbox.
- Rollback when any insert/update fails.
- Foreign-key and uniqueness constraints.
- Register-local sequence handling.
- Outbox replay and idempotent server ingestion.
- Pull cursor handling and incremental updates.
- Master-data version conflict handling.
- Multi-tenant isolation.
- Backup and restore.

## 16.3 Simulator and end-to-end tests

- Virtual scanner burst scans, malformed data, repeated scans, unknown codes, and disconnects.
- Virtual printer success, failure, timeout, paper-out, and retry.
- Mock payment approval, decline, timeout, delayed callback, and duplicate callback.
- Online/offline transitions during checkout and during sync.
- Forced application termination before, during, and after local sale commit.
- Device clock drift and timezone/report-day boundaries.
- CSV imports with duplicate barcodes, missing prices, invalid tax categories, and malformed rows.

## 16.4 Acceptance tests for MVP

1. A new business can create a profile, add at least 20 products, and complete a test cash sale without cloud connectivity.
2. A product can be found by barcode, SKU, and name.
3. Repeated barcode scans increment quantity correctly and do not lose legitimate rapid scans.
4. A completed sale and all dependent local records commit atomically.
5. Restarting the application does not lose committed sales or pending sync events.
6. A sale created offline synchronizes after reconnection exactly once, even if the client retries the same event several times.
7. A conflicting product edit is surfaced; it is not silently overwritten.
8. A receipt can be previewed and exported to PDF without a physical printer.
9. ESC/POS output can be captured and tested using a virtual printer.
10. A return creates a linked return/refund record and the correct stock movement.
11. A cashier cannot perform a manager-only operation by directly invoking an IPC command.
12. A business cannot read another business's cloud data.
13. The application clearly distinguishes pending synchronization from synchronized data and external payment confirmation from processor approval.

# 17. Delivery roadmap

## Stage 0 - Architecture and technical spikes

- Finalize domain boundaries and shared Rust crate layout.
- Validate Tauri 2 packaging on Windows.
- Validate SQLite migrations, local transactions, and backup.
- Validate Rust domain crate reuse in Axum cloud API.
- Define OpenAPI and event schemas.
- Prototype barcode keyboard input capture and virtual printer capture.

**Exit gate:** one desktop shell can create a local database, store a product, simulate a barcode scan, and commit a demo sale atomically.

## Stage 1 - Catalogue and checkout foundation

- Business setup and basic login.
- Product/category/variant/barcode CRUD.
- CSV import.
- Local search and barcode simulator.
- Cart and pricing engine.
- Tax and discount rules.

**Exit gate:** fast local checkout in demo mode with no cloud and no physical hardware.

## Stage 2 - Sales, stock, and receipts

- Completed sale model and receipt numbering.
- Cash and split tender.
- Manual external tender record.
- Inventory ledger and balance projections.
- Stock receiving and adjustment.
- Returns/refunds.
- PDF receipt and virtual ESC/POS printer.

**Exit gate:** a complete local operating day can be run and reconciled on one desktop.

## Stage 3 - Offline cloud synchronization

- Cloud account and device registration.
- Bootstrap and incremental master-data pull.
- Transactional outbox.
- Idempotent server event ingestion.
- Conflict detection and resolution.
- Cloud backup and consolidated reporting.

**Exit gate:** disconnected sales replay correctly after reconnection with no duplicate business effects.

## Stage 4 - Pilot hardening

- Performance benchmarks.
- Crash and recovery testing.
- Security review.
- Data export and restore testing.
- Staff usability testing in representative shops.
- Signed installers and update workflow.
- Support/diagnostics bundle.

**Exit gate:** production pilot acceptance criteria pass on documented Windows hardware.

## Stage 5 - Physical hardware validation (after core MVP)

- Acquire a small test matrix of USB HID scanners and ESC/POS printers.
- Test supported Windows versions and connection methods.
- Validate printer width, cut, paper status, and drawer kick where applicable.
- Add hardware compatibility registry and adapter release process.
- Add scale and payment provider integrations only when real devices, SDKs, test credentials, and commercial agreements are available.

# 18. Risks and mitigations

| Risk | Why it matters | Mitigation |
|---|---|---|
| Offline master-data conflicts | Two devices may edit the same product while disconnected | Versioned updates, conflict queue, manager resolution |
| Duplicate sync delivery | Network retries can repeat a completed sale | Unique event IDs, idempotent inbox, atomic server acknowledgement |
| Concurrent offline stock sales | Registers may sell more than the latest known stock | Accept/flag negative stock in MVP; defer strict offline reservation |
| Wrong tax/rounding | Financial totals and invoices can be incorrect | Decimal-safe domain rules, snapshot calculations, jurisdiction review |
| Printer command variability | ESC/POS-labelled devices do not always implement identical commands | Adapter capability profiles and physical certification tests |
| User deletes or damages local data | Offline sales may be lost | Automatic local backup, encrypted cloud sync, restore drills |
| Misleading payment state | Cashier may believe an unapproved card payment succeeded | Explicit external/manual vs provider-confirmed states |
| Excessive scope | Product becomes an ERP before core checkout is reliable | Scope guardrail and release non-goals |
| Tenant data exposure | SaaS business data is sensitive | Server-derived tenant identity, database constraints/RLS, security tests |

# 19. Product decisions and open questions

## Decisions fixed by this PRD

- Desktop-first product.
- Offline-first with cloud synchronization.
- Windows 11 is the first production pilot target; keep architecture cross-platform. Windows 10 is not a default supported target because mainstream support ended on 14 October 2025; consider only explicitly supported ESU/LTSC environments.
- One business and one initial location per workspace.
- Minimal owner/manager/cashier roles.
- No dedicated staff/HR or CRM module.
- No restaurant, KDS, appointments, payroll, or advanced ERP in MVP.
- Software simulators for scanner, printer, payment, and network conditions.
- No physical peripheral is required to complete core development or use manual checkout.
- Local SQLite and cloud PostgreSQL.
- Tauri 2 + React/TypeScript + Rust core; Rust/Axum cloud API.

## Decisions to finalize before implementation

1. Product branding and application name.
2. First pilot country and exact tax/invoice compliance profile.
3. Whether MVP requires one or multiple concurrent registers per location in the pilot; the data model supports multiple, but offline stock policy must be communicated.
4. Whether cloud account creation is mandatory before the first local sale or can be linked later.
5. First supported Windows versions and minimum hardware profile.
6. Initial cloud hosting region, backup retention, and operational budget.
7. First physical scanner and printer models to purchase for Stage 5.
8. Whether a real payment provider is required for pilot or cash/manual external tender is acceptable.

# 20. Definition of done

The MVP is complete when all P0 requirements are implemented, acceptance tests pass, security and tenant-isolation tests pass, the application can complete an operating day offline, sync retries do not duplicate any business event, data can be backed up and restored, receipts work through PDF and virtual printer paths, and the first production pilot passes on the documented Windows hardware profile.

A feature is not complete merely because its happy-path screen exists. It must have domain validation, persistent storage, permission enforcement, observable errors, offline behaviour where applicable, and automated tests.

# 21. Technical references

The following sources inform implementation constraints and interoperability. They should be rechecked at implementation time because platform documentation and device behaviour can change.

1. **Tauri - Architecture:** https://v2.tauri.app/concept/architecture/ - native Rust layer, operating-system WebView, and message passing.
2. **Tauri - Capabilities:** https://v2.tauri.app/security/capabilities/ - fine-grained window/webview permissions.
3. **Tauri - SQL plugin:** https://v2.tauri.app/plugin/sql/ - documented SQL plugin and supported databases. This PRD recommends keeping core business transactions behind Rust application commands rather than letting UI components issue arbitrary SQL.
4. **SQLite - Write-Ahead Logging:** https://www.sqlite.org/wal.html - WAL concurrency and checkpoint considerations.
5. **SQLite - Transactions:** https://www.sqlite.org/lang_transaction.html - transaction boundaries and commit/rollback behaviour.
6. **PostgreSQL - Row Security Policies:** https://www.postgresql.org/docs/current/ddl-rowsecurity.html - tenant isolation defense in depth.
7. **GS1 - Barcodes:** https://www.gs1.org/standards/barcodes - barcode carriers, identifiers, attributes, and GS1 syntax.
8. **Epson - ePOS-Print API User's Manual:** https://files.support.epson.com/pdf/pos/bulk/tm-i_epos-print_um_en_revk.pdf - example of manufacturer-specific printer APIs and capabilities.
9. **PostgreSQL - Constraints:** https://www.postgresql.org/docs/current/ddl-constraints.html - uniqueness and data integrity constraints.

---

**End of document**