//! E14-T01/T02: report boundaries, scope, stock statuses and the 500k-line target. The
//! fixture day's figures are checked in `end_of_day.rs`.

mod common;

use pos_application::cart::CartService;
use pos_application::catalog::CatalogService;
use pos_application::checkout::{CheckoutService, CompleteSale};
use pos_application::queries::SalesFilter;
use pos_application::reports::{ReportService, StockStatus, TopBy};
use pos_application::sales::SalesService;
use pos_application::scan::ScanSource;
use pos_domain::catalog::{ProductInput, ProductType, Unit, VariantInput};
use pos_domain::settlement::{TenderInput, TenderKind};
use pos_domain::time::BusinessDate;
use pos_domain::{ErrorCode, HasErrorCode, Money, Quantity, Role, ScanEventId, Timestamp, UuidV7Generator};
use std::sync::Arc;
use std::time::{Duration, Instant};

async fn product(shop: &common::Shop, name: &str, price: i64, threshold: Option<i64>) {
    let mut v = VariantInput::simple(Money::from_minor(price));
    v.sku = Some(name.to_uppercase());
    v.low_stock_threshold = threshold.map(Quantity::from_milli);
    CatalogService {
        store: Arc::new(shop.store.clone()),
        catalog: Arc::new(shop.store.clone()),
    }
    .create(
        &shop.owner,
        ProductInput {
            name: name.into(),
            description: None,
            category_id: None,
            product_type: ProductType::Stock,
            unit: Unit::Ea,
            tax_category_id: shop.tax_category,
            quick_select: false,
            variants: vec![v],
        },
    )
    .await
    .unwrap();
}

async fn sell(shop: &common::Shop, carts: &Arc<CartService>, sku: &str) {
    carts
        .scan(
            &shop.owner,
            sku,
            ScanEventId::generate(&UuidV7Generator),
            ScanSource::Manual,
        )
        .await
        .unwrap();
    let cart = carts.current(&shop.owner).await.unwrap().cart;
    let s = shop.store.clone();
    CheckoutService {
        carts: carts.clone(),
        store: Arc::new(s.clone()),
        setup: Arc::new(s.clone()),
        stock: Arc::new(s.clone()),
        receipts: Arc::new(s),
    }
    .complete_sale(
        &shop.owner,
        CompleteSale {
            cart_id: cart.id,
            revision: cart.revision,
            tenders: vec![TenderInput {
                kind: TenderKind::Cash,
                amount: Money::from_minor(10_000),
                external_reference: None,
            }],
        },
    )
    .await
    .unwrap();
}

fn day(s: &str) -> BusinessDate {
    s.parse().unwrap()
}

async fn exec(shop: &common::Shop, sql: &str) {
    sqlx::query(sql).execute(shop.store.db().writer()).await.unwrap();
}

#[tokio::test]
async fn business_days_are_local_and_practice_sales_never_count() {
    let shop = common::shop().await;
    // A business in Kolkata (UTC+5:30), live, selling without a drawer session.
    exec(&shop, "UPDATE businesses SET timezone = 'Asia/Kolkata'").await;
    exec(&shop, "INSERT OR REPLACE INTO business_settings (business_id, key, value_json, updated_at) SELECT business_id, 'cash.require_session_for_sale', 'false', 0 FROM businesses").await;
    product(&shop, "Tea", 250, None).await;
    let carts = Arc::new(CartService::new(
        Arc::new(shop.store.clone()),
        Arc::new(shop.store.clone()),
        Arc::new(shop.store.clone()),
    ));
    // A practice sale first: excluded.
    shop.clock.set(Timestamp::from_millis(1_767_265_200_000)); // 2026-01-01T11:00Z
    sell(&shop, &carts, "TEA").await;
    exec(&shop, "UPDATE device_identity SET mode = 'live'").await;
    // 18:00 UTC is 23:30 in Kolkata: still 1 January there.
    shop.clock.set(Timestamp::from_millis(1_767_290_400_000)); // 2026-01-01T18:00Z
    sell(&shop, &carts, "TEA").await;
    // 18:45 UTC is 00:15 on 2 January in Kolkata (still 1 January in UTC).
    shop.clock.set(Timestamp::from_millis(1_767_293_100_000)); // 2026-01-01T18:45Z
    sell(&shop, &carts, "TEA").await;
    sell(&shop, &carts, "TEA").await;

    let reports = ReportService {
        reports: Arc::new(shop.store.clone()),
    };
    let owner = &shop.owner;
    let summary = reports
        .sales_summary(owner, day("2026-01-01"), day("2026-01-02"))
        .await
        .unwrap();
    let per_day: Vec<(String, u32, i64)> = summary
        .days
        .iter()
        .map(|d| {
            (
                d.date.to_string(),
                d.figures.sales_count,
                d.figures.sales_total.minor(),
            )
        })
        .collect();
    assert_eq!(
        per_day,
        [
            ("2026-01-01".to_owned(), 1, 250),
            ("2026-01-02".to_owned(), 2, 500)
        ]
    );

    // The report reconciles with the sales list for the same range (live sales only there
    // too once the practice sale is set aside).
    let list = SalesService {
        sales: Arc::new(shop.store.clone()),
    }
    .list(
        owner,
        &SalesFilter {
            from: Some(day("2026-01-01")),
            to: Some(day("2026-01-02")),
            ..SalesFilter::default()
        },
        None,
        Some(200),
    )
    .await
    .unwrap();
    let live_total: i64 = list
        .rows
        .iter()
        .filter(|r| r.mode == pos_domain::sale::SaleMode::Live)
        .map(|r| r.total.minor())
        .sum();
    assert_eq!(live_total, summary.total.sales_total.minor());
    assert_eq!(list.rows.len(), 4, "the list shows the practice sale too, marked");

    // Ranges: backwards or longer than a year are refused; a cashier cannot see reports.
    assert!(
        reports
            .sales_summary(owner, day("2026-01-02"), day("2026-01-01"))
            .await
            .is_err()
    );
    assert!(
        reports
            .sales_summary(owner, day("2025-01-01"), day("2026-01-02"))
            .await
            .is_err()
    );
    assert_eq!(
        reports
            .sales_summary(&shop.as_role(Role::Cashier), day("2026-01-01"), day("2026-01-01"))
            .await
            .unwrap_err()
            .error_code(),
        ErrorCode::PermissionDenied
    );
}

#[tokio::test]
async fn the_stock_report_flags_low_and_negative() {
    let shop = common::shop().await;
    product(&shop, "Apples", 50, Some(5_000)).await; // receive 5: low (at the threshold)
    product(&shop, "Bananas", 30, Some(5_000)).await; // receive 12: ok
    product(&shop, "Cherries", 90, None).await; // sell 1 with none: negative
    product(&shop, "Dates", 70, None).await; // nothing at all: ok, 0
    let stock = pos_application::stock::StockService {
        store: Arc::new(shop.store.clone()),
        carts: Arc::new(shop.store.clone()),
        stock: Arc::new(shop.store.clone()),
    };
    let reports = ReportService {
        reports: Arc::new(shop.store.clone()),
    };
    let all = reports.stock(&shop.owner, false).await.unwrap();
    let id = |n: &str| all.iter().find(|r| r.name == n).unwrap().variant_id;
    for (name, milli) in [("Apples", 5_000), ("Bananas", 12_000)] {
        stock
            .post(
                &shop.owner,
                pos_domain::stock_docs::DocumentDraft {
                    doc_type: pos_domain::stock_docs::DocumentType::Receipt,
                    reason: None,
                    supplier_name: None,
                    reference: None,
                    note: None,
                    lines: vec![pos_domain::stock_docs::DocumentLine {
                        variant_id: id(name),
                        quantity: Quantity::from_milli(milli),
                        unit_cost: None,
                    }],
                },
            )
            .await
            .unwrap();
    }
    let carts = Arc::new(CartService::new(
        Arc::new(shop.store.clone()),
        Arc::new(shop.store.clone()),
        Arc::new(shop.store.clone()),
    ));
    sell(&shop, &carts, "CHERRIES").await;

    let rows: Vec<(String, i64, StockStatus)> = reports
        .stock(&shop.owner, false)
        .await
        .unwrap()
        .into_iter()
        .map(|r| (r.name, r.sellable.milli(), r.status))
        .collect();
    assert_eq!(
        rows,
        [
            ("Apples".to_owned(), 5_000, StockStatus::Low),
            ("Bananas".to_owned(), 12_000, StockStatus::Ok),
            ("Cherries".to_owned(), -1_000, StockStatus::Negative),
            ("Dates".to_owned(), 0, StockStatus::Ok),
        ]
    );
    let low: Vec<String> = reports
        .stock(&shop.owner, true)
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.name)
        .collect();
    assert_eq!(low, ["Apples", "Cherries"]);
    // Sandbox sales are left out of sales reports, but stock is stock.
    let top = reports
        .top_products(
            &shop.owner,
            day("2026-01-01"),
            day("2026-01-01"),
            TopBy::Quantity,
            5,
        )
        .await
        .unwrap();
    assert!(top.is_empty(), "the only sale was a practice sale");
}

#[tokio::test]
async fn a_30_day_summary_and_top_products_are_fast_at_500k_lines() {
    let shop = common::shop().await;
    let reg = &shop.owner.register;
    let cashier = shop.owner.actor.as_ref().unwrap().user_id;
    let start = 1_735_689_600_000_i64; // 2025-01-01T00:00Z
    let mut tx = shop.store.db().writer().begin().await.unwrap();
    sqlx::query(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 125000)
         INSERT INTO sales (sale_id, business_id, location_id, register_id, receipt_seq, receipt_number, mode,
           currency_code, currency_exponent, prices_include_tax, subtotal_minor, line_discount_total_minor,
           order_discount_total_minor, tax_total_minor, total_minor, tendered_total_minor, change_minor,
           tax_breakdown_json, cashier_user_id, occurred_at, business_date, timezone, created_at)
         SELECT printf('00000000-0000-7000-8000-%012d', i), ?1, ?2, ?3, 1000 + i, printf('R01-%06d', 1000 + i), 'live',
           'GBP', 2, 1, 1000, 0, 0, 167, 1000, 1000, 0, '[]', ?4,
           ?5 + i * 252288, date((?5 + i * 252288) / 1000, 'unixepoch'), 'UTC', ?5 + i * 252288
         FROM n",
    )
    .bind(reg.business_id.to_string())
    .bind(reg.location_id.to_string())
    .bind(reg.register_id.to_string())
    .bind(cashier.to_string())
    .bind(start)
    .execute(&mut *tx)
    .await
    .unwrap();
    // 4 lines per sale over 200 products.
    sqlx::query(
        "WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i < 499999)
         INSERT INTO sale_lines (sale_line_id, sale_id, business_id, line_no, product_id, variant_id, name_snapshot,
           variant_label_snapshot, unit_code, product_type, quantity_milli, unit_price_minor, gross_minor,
           line_discount_minor, order_discount_alloc_minor, tax_minor, total_minor, tax_lines_json)
         SELECT printf('00000000-0000-7000-9000-%012d', i), printf('00000000-0000-7000-8000-%012d', i / 4 + 1), ?1,
           i % 4 + 1, 'p', printf('00000000-0000-7000-a000-%012d', (i * 7) % 200), printf('Item %d', (i * 7) % 200), '',
           'ea', 'stock', 1000, 250, 250, 0, 0, 42, 250, '[]'
         FROM n",
    )
    .bind(reg.business_id.to_string())
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO tenders (tender_id, sale_id, business_id, seq, tender_type, amount_minor, status,
           confirmation_source, recorded_by, occurred_at)
         SELECT 't' || sale_id, sale_id, business_id, 1, CASE WHEN receipt_seq % 3 = 0 THEN 'card_external' ELSE 'cash' END,
           total_minor, 'confirmed', 'operator', cashier_user_id, occurred_at
         FROM sales WHERE receipt_seq > 1000",
    )
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    sqlx::query("ANALYZE")
        .execute(shop.store.db().writer())
        .await
        .unwrap();

    let reports = ReportService {
        reports: Arc::new(shop.store.clone()),
    };
    let (from, to) = (day("2025-06-01"), day("2025-06-30"));
    let owner = &shop.owner;
    let timed = |budget: Duration| {
        move |started: Instant, what: &str| {
            let took = started.elapsed();
            eprintln!("{what}: {took:?}");
            assert!(took < budget, "{what}: {took:?}");
        }
    };
    // Warm once (statement cache, page cache), then time.
    reports.sales_summary(owner, from, to).await.unwrap();
    let t0 = Instant::now();
    let summary = reports.sales_summary(owner, from, to).await.unwrap();
    timed(Duration::from_secs(1))(t0, "30-day summary");
    assert_eq!(summary.days.len(), 30);
    assert!(summary.total.sales_count > 10_000);
    let t0 = Instant::now();
    let mix = reports.tender_mix(owner, from, to).await.unwrap();
    timed(Duration::from_secs(1))(t0, "30-day tender mix");
    assert_eq!(mix.len(), 2);
    reports
        .top_products(owner, from, to, TopBy::Value, 10)
        .await
        .unwrap();
    let t0 = Instant::now();
    let top = reports
        .top_products(owner, from, to, TopBy::Value, 10)
        .await
        .unwrap();
    timed(Duration::from_millis(1500))(t0, "30-day top products");
    assert_eq!(top.len(), 10);
}
