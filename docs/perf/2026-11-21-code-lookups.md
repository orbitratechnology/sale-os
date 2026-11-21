# Barcode and SKU lookups walked their index (fixed 2026-11-21)

Found while timing `cargo xtask seed --profile bench50k` (E00-T04): 50,000 variants took 541 s
to create through `CatalogService`, and the time per product grew with the catalogue.

**Cause.** The unique indexes on barcodes and SKUs lead with `business_id`
(`ux_barcode_value (business_id, value_normalized)`, `ux_variant_sku (business_id, sku)`), and
the four code lookups (`barcode_owner`, `sku_owner`, the SKU match in `lookup_code`, and
`barcode_hit` behind every scan) did not name the business. SQLite then walked the whole index
for each lookup: 6.4 ms per barcode at 100,000 barcodes, against 0.006 ms with the business
named. A scan tries several decoded candidates, and an unknown code tries them all.

**Why the benchmark missed it.** `pos-bench` ran `ANALYZE` after building its database. With
those statistics SQLite skip-scanned the single `business_id` and the lookup looked cheap
(0.1 ms in `2026-11-17.md`). A till never gathers statistics, so it got the full walk.
`pos-bench` no longer runs `ANALYZE`.

**Fix.** Each lookup now says `business_id IN (SELECT business_id FROM businesses)` (a till
holds one business). `catalog_queries::tests::queries_search_their_unique_index` checks that
each plan searches its unique index and scans nothing.

## Before and after

`pos-bench --scale full --samples 300`, release build, no planner statistics (as on a till),
same machine as `2026-11-17.md`. Encrypted database as in the app.

| Measure | p50 before | p50 after | p95 before | p95 after |
|---|---|---|---|---|
| Barcode lookup | 3.9 ms | 0.2 ms | 6.6 ms | 0.6 ms |
| Add item to visible cart | 20.4 ms | 8.6 ms | 36.8 ms | 13.5 ms |
| Product search | 19.6 ms | 20.0 ms | 31.7 ms | 35.0 ms |
| Cash-sale local commit | 7.8 ms | 8.0 ms | 12.3 ms | 13.7 ms |

The other measures are unchanged. Seeding bench50k: 541 s before, 68 s after. Full results:
`2026-11-21.md`. All PRD §14 targets were met before and after; on the slower reference machine
(D-05) the old scan cost would have taken a larger share of the 100 ms budget.
