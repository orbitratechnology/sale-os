# Post-MVP Backlog (P1 / P2)

These items are deliberately out of the MVP path. The order is a suggestion. Re-rank it after
pilot feedback.

Each entry names the **extension point already built**, so that adding it later does not
need a redesign.

| Order | PRD ID | Item | Prio | Extension point already in place |
|---|---|---|---|---|
| 1 | POS-012 | Park and retrieve carts | P1 | `carts.status = 'parked'` is already allowed. Relax the unique-open-cart index to open-only (already the case). |
| 2 | INV-010 | Cycle counts and stocktake reconciliation | P1 | New `inventory_documents.doc_type = 'count'`, which produces `correction` movements |
| 3 | REP-007 | Cost-based gross margin | P1 | `sale_lines.unit_cost_minor` is already snapshotted. Needs a valuation policy ADR (average or FIFO). |
| 4 | PAY-007 / PAY-008 | Real payment provider and reconciliation | P1 | `PaymentPort`, `tender_status_events`, `confirmation_source = provider`, and the E10-T02 state machine |
| 5 | IAM-006 | Account recovery and MFA | P1 | `IdentityProvider` trait (D-09: move to managed OIDC) |
| 6 | CAT-010 | Product images through object storage | P1 | Image path column, plus an object-storage upload worker |
| 7 | SYN-011 / SET-007 | Additional locations and stock transfers | P1 | `location_id` is already on every movement. Add a `transfer` document type with paired out/in movements. |
| 8 | REP-008 | Full consolidated cloud reporting (beyond the E14-T05 minimum) | P1 | `daily_sales_rollup`, `completeness` |
| 9 | SET-006 | Price lists and customer-specific pricing | P1 | `price_cart` takes resolved unit prices. Add a `PriceResolver` ahead of it. |
| 10 | POS-013 | Quotations, converted to sales | P1 | A cart with `status = 'quote'` and an expiry |
| 11 | BAR-009 | GS1 AI parsing, variable-measure barcodes, camera decoding, serial scanners | P2 | `normalize_barcode` and the `Symbology` enum. Variable-measure barcodes (price/weight embedded) become a new `BarcodeQualifier`. |
| 12 | INV-011 | Lot, expiry, serial/IMEI and warranty | P2 | Optional `tracking_json` on sale and movement lines. A new tracked-variant flag. |
| 13 | POS-014 | Store credit, gift cards, layaway, customer accounts | P2 | A new `TenderKind`, plus a customer aggregate |
| 14 | RCP-009 | Star and other vendor adapters, label printers | P2 | `ReceiptOutputPort`, capability profiles |
| 15 | — | Web back-office | P2 | Cloud REST API + OpenAPI |
| 16 | — | Local store gateway for strict multi-register stock reservation | P2 | Sync protocol: the gateway would act as a cloud-compatible ingestion endpoint on the LAN |
