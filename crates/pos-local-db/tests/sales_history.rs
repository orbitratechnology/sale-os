//! E12-T03: sales history and sale detail against a real database, including the
//! NFR-sales-history target (< 300 ms per page at 500k sale lines).

mod common;

use pos_application::cart::CartService;
use pos_application::catalog::CatalogService;
use pos_application::checkout::{CheckoutService, CompleteSale};
use pos_application::queries::SalesFilter;
use pos_application::returns::{CreateReturn, ReturnService};
use pos_application::sales::SalesService;
use pos_application::scan::ScanSource;
use pos_domain::catalog::{ProductInput, ProductType, Unit, VariantInput};
use pos_domain::returns::{Disposition, ReturnLineRequest, ReturnReason};
use pos_domain::settlement::{TenderInput, TenderKind};
use pos_domain::{ErrorCode, HasErrorCode, Money, Quantity, Role, ScanEventId, UserId, UuidV7Generator};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn tender(kind: TenderKind, minor: i64, reference: Option<&str>) -> TenderInput {
    TenderInput {
        kind,
        amount: Money::from_minor(minor),
        external_reference: reference.map(Into::into),
    }
}

struct Till {
    shop: common::Shop,
    carts: Arc<CartService>,
    checkout: CheckoutService,
    sales: SalesService,
}

async fn till() -> Till {
    let shop = common::shop().await;
    let s = shop.store.clone();
    let catalog = CatalogService {
        store: Arc::new(s.clone()),
        catalog: Arc::new(s.clone()),
    };
    for (name, price, sku) in [("Tea", 250, "TEA"), ("Cake", 400, "CAKE")] {
        let mut v = VariantInput::simple(Money::from_minor(price));
        v.sku = Some(sku.into());
        catalog
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
    let carts = Arc::new(CartService::new(
        Arc::new(s.clone()),
        Arc::new(s.clone()),
        Arc::new(s.clone()),
    ));
    Till {
        checkout: CheckoutService {
            carts: carts.clone(),
            store: Arc::new(s.clone()),
            setup: Arc::new(s.clone()),
            stock: Arc::new(s.clone()),
            receipts: Arc::new(s.clone()),
        },
        sales: SalesService { sales: Arc::new(s) },
        carts,
        shop,
    }
}

/// Scans `codes` and pays with `tenders`; returns the receipt number. A minute passes after.
async fn sell(t: &Till, codes: &[&str], tenders: Vec<TenderInput>) -> String {
    for code in codes {
        t.carts
            .scan(
                &t.shop.owner,
                code,
                ScanEventId::generate(&UuidV7Generator),
                ScanSource::Manual,
            )
            .await
            .unwrap();
    }
    let cart = t.carts.current(&t.shop.owner).await.unwrap().cart;
    let done = t
        .checkout
        .complete_sale(
            &t.shop.owner,
            CompleteSale {
                cart_id: cart.id,
                revision: cart.revision,
                tenders,
            },
        )
        .await
        .unwrap();
    t.shop.clock.advance_ms(60_000);
    done.receipt_number
}

fn numbers(page: &pos_application::records::SalesPage) -> Vec<&str> {
    page.rows.iter().map(|r| r.receipt_number.as_str()).collect()
}

#[tokio::test]
async fn the_list_is_newest_first_filtered_and_paged_and_the_detail_shows_everything() {
    let t = till().await;
    let owner = &t.shop.owner;
    sell(&t, &["TEA"], vec![tender(TenderKind::Cash, 500, None)]).await; // 2.50
    sell(
        &t,
        &["TEA", "CAKE"],
        vec![tender(TenderKind::CardExternal, 650, Some("AUTH 1"))],
    )
    .await;
    sell(
        &t,
        &["CAKE", "CAKE", "TEA"],
        vec![
            tender(TenderKind::CardExternal, 500, None),
            tender(TenderKind::Cash, 1000, None),
        ],
    )
    .await; // 10.50

    let all = t
        .sales
        .list(owner, &SalesFilter::default(), None, None)
        .await
        .unwrap();
    assert_eq!(numbers(&all), ["R01-000003", "R01-000002", "R01-000001"]);
    assert!(all.next.is_none());
    let top = &all.rows[0];
    assert_eq!((top.total.minor(), top.item_count.milli()), (1050, 3000));
    assert_eq!(top.tenders, [TenderKind::CardExternal, TenderKind::Cash]);
    assert_eq!(top.cashier_name, "Olu");
    assert_eq!(top.local_time, "2026-01-01 00:02");

    // Pages of 2: a token for the next page, then the rest, then nothing more.
    let first = t
        .sales
        .list(owner, &SalesFilter::default(), None, Some(2))
        .await
        .unwrap();
    assert_eq!(numbers(&first), ["R01-000003", "R01-000002"]);
    let token = first.next.as_ref().unwrap().encode();
    let second = t
        .sales
        .list(owner, &SalesFilter::default(), Some(&token), Some(2))
        .await
        .unwrap();
    assert_eq!(numbers(&second), ["R01-000001"]);
    assert!(second.next.is_none());
    assert_eq!(
        t.sales
            .list(owner, &SalesFilter::default(), Some("nonsense"), None)
            .await
            .unwrap_err()
            .error_code(),
        ErrorCode::ValidationFailed
    );

    // Filters: receipt number (any case, or just the digits), tender, cashier, dates.
    let by = |f: SalesFilter| {
        let s = &t.sales;
        async move { s.list(owner, &f, None, None).await.unwrap() }
    };
    let f = |receipt: Option<&str>, tender: Option<TenderKind>, cashier: Option<UserId>| SalesFilter {
        receipt: receipt.map(Into::into),
        tender,
        cashier,
        ..SalesFilter::default()
    };
    assert_eq!(
        numbers(&by(f(Some(" r01-000002 "), None, None)).await),
        ["R01-000002"]
    );
    assert_eq!(numbers(&by(f(Some("1"), None, None)).await), ["R01-000001"]);
    assert_eq!(
        numbers(&by(f(None, Some(TenderKind::CardExternal), None)).await),
        ["R01-000003", "R01-000002"]
    );
    assert!(
        by(f(None, Some(TenderKind::QrExternal), None))
            .await
            .rows
            .is_empty()
    );
    let olu = owner.actor.as_ref().unwrap().user_id;
    assert_eq!(by(f(None, None, Some(olu))).await.rows.len(), 3);
    assert!(
        by(f(None, None, Some(UserId::generate(&UuidV7Generator))))
            .await
            .rows
            .is_empty()
    );
    let day = |d: &str| Some(d.parse().unwrap());
    let range = |from, to| SalesFilter {
        from,
        to,
        ..SalesFilter::default()
    };
    assert_eq!(
        by(range(day("2026-01-01"), day("2026-01-01"))).await.rows.len(),
        3
    );
    assert!(by(range(day("2026-01-02"), None)).await.rows.is_empty());
    assert!(
        t.sales
            .list(owner, &range(day("2026-01-02"), day("2026-01-01")), None, None)
            .await
            .is_err()
    );

    // A return shows in the list (refunded) and in the detail.
    let s = t.shop.store.clone();
    let returns = ReturnService {
        store: Arc::new(s.clone()),
        returns: Arc::new(s.clone()),
        setup: Arc::new(s.clone()),
        receipts: Arc::new(s.clone()),
        pricing: Arc::new(s.clone()),
    };
    let sale = returns.find(owner, "R01-000003").await.unwrap();
    let cake = sale.lines.iter().find(|l| l.name == "Cake").unwrap();
    returns
        .create(
            owner,
            CreateReturn {
                sale_id: sale.sale_id,
                lines: vec![ReturnLineRequest {
                    sale_line_id: cake.sale_line_id,
                    quantity: Quantity::from_milli(1000),
                    disposition: Disposition::Restock,
                }],
                reason: ReturnReason::ChangedMind,
                note: None,
                refunds: vec![tender(TenderKind::Cash, 400, None)],
            },
        )
        .await
        .unwrap();
    let all = t
        .sales
        .list(owner, &SalesFilter::default(), None, None)
        .await
        .unwrap();
    assert_eq!(all.rows[0].refunded.minor(), 400);

    let d = t.sales.get(owner, sale.sale_id).await.unwrap();
    assert_eq!(d.receipt_number, "R01-000003");
    assert_eq!(
        (d.total.minor(), d.tendered.minor(), d.change.minor()),
        (1050, 1500, 450)
    );
    assert_eq!(d.lines.len(), 2);
    let cake = d.lines.iter().find(|l| l.name == "Cake").unwrap();
    assert_eq!(
        (cake.quantity.milli(), cake.returned.milli(), cake.total.minor()),
        (2000, 1000, 800)
    );
    assert_eq!(d.tax_breakdown.len(), 1);
    assert_eq!(d.tax_breakdown[0].rate_ppm, 200_000);
    assert_eq!(d.tax_breakdown[0].tax.minor(), d.tax_total.minor());
    assert_eq!(d.tenders.len(), 2);
    assert!(
        d.tenders
            .iter()
            .all(|t| t.confirmation_source == "operator" && t.status == "confirmed")
    );
    assert_eq!(d.tenders[1].received.map(|m| m.minor()), Some(1000));
    assert_eq!(d.returns.len(), 1);
    assert_eq!(d.returns[0].return_number, "R01-R-000001");
    assert_eq!(d.returns[0].refunds, [TenderKind::Cash]);
    let card = t
        .sales
        .list(owner, &f(Some("2"), None, None), None, None)
        .await
        .unwrap();
    let card = t.sales.get(owner, card.rows[0].sale_id).await.unwrap();
    assert_eq!(card.tenders[0].reference.as_deref(), Some("AUTH 1"));

    // Every role may look (SalesView); a missing sale is a validation error.
    let cashier = t.shop.as_role(Role::Cashier);
    assert!(
        t.sales
            .list(&cashier, &SalesFilter::default(), None, None)
            .await
            .is_ok()
    );
    assert!(
        t.sales
            .get(owner, pos_domain::SaleId::generate(&UuidV7Generator))
            .await
            .is_err()
    );
}

/// 125,000 sales of 4 lines (500k lines) over 365 days, written straight into the tables.
async fn history_500k(t: &Till) {
    let reg = &t.shop.owner.register;
    let cashier = t.shop.owner.actor.as_ref().unwrap().user_id;
    let start = 1_735_689_600_000_i64; // 2025-01-01T00:00Z
    let mut tx = t.shop.store.db().writer().begin().await.unwrap();
    // Sale n at start + n * ~252 s, so 125k sales span 365 days.
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
    sqlx::query(
        "WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i < 499999)
         INSERT INTO sale_lines (sale_line_id, sale_id, business_id, line_no, product_id, variant_id, name_snapshot,
           variant_label_snapshot, unit_code, product_type, quantity_milli, unit_price_minor, gross_minor,
           line_discount_minor, order_discount_alloc_minor, tax_minor, total_minor, tax_lines_json)
         SELECT printf('00000000-0000-7000-9000-%012d', i), printf('00000000-0000-7000-8000-%012d', i / 4 + 1), ?1,
           i % 4 + 1, 'p', 'v', 'Item', '', 'ea', 'stock', 1000, 250, 250, 0, 0, 42, 250, '[]'
         FROM n",
    )
    .bind(reg.business_id.to_string())
    .execute(&mut *tx)
    .await
    .unwrap();
    // One tender per sale: every 50th is a QR payment, every 3rd a card, the rest cash.
    sqlx::query(
        "INSERT INTO tenders (tender_id, sale_id, business_id, seq, tender_type, amount_minor, status,
           confirmation_source, recorded_by, occurred_at)
         SELECT 't' || sale_id, sale_id, business_id, 1,
           CASE WHEN receipt_seq % 50 = 0 THEN 'qr_external' WHEN receipt_seq % 3 = 0 THEN 'card_external' ELSE 'cash' END,
           total_minor, 'confirmed', 'operator', cashier_user_id, occurred_at
         FROM sales WHERE receipt_seq > 1000",
    )
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    sqlx::query("ANALYZE")
        .execute(t.shop.store.db().writer())
        .await
        .unwrap();
}

#[tokio::test]
async fn a_page_takes_under_300_ms_with_500k_sale_lines() {
    let t = till().await;
    let built = Instant::now();
    history_500k(&t).await;
    eprintln!("500k lines written in {:?}", built.elapsed());
    let owner = &t.shop.owner;
    let olu = owner.actor.as_ref().unwrap().user_id;
    let day = |d: &str| Some(d.parse().unwrap());

    // The first page, a deep page (via tokens), and each filter.
    let mut cases: Vec<(&str, SalesFilter)> = vec![
        ("newest", SalesFilter::default()),
        (
            "one day in March",
            SalesFilter {
                from: day("2025-03-14"),
                to: day("2025-03-14"),
                ..SalesFilter::default()
            },
        ),
        (
            "a quarter",
            SalesFilter {
                from: day("2025-04-01"),
                to: day("2025-06-30"),
                ..SalesFilter::default()
            },
        ),
        (
            "receipt digits",
            SalesFilter {
                receipt: Some("62000".into()),
                ..SalesFilter::default()
            },
        ),
        (
            "receipt number",
            SalesFilter {
                receipt: Some("r01-062000".into()),
                ..SalesFilter::default()
            },
        ),
        (
            "cashier",
            SalesFilter {
                cashier: Some(olu),
                ..SalesFilter::default()
            },
        ),
        (
            "QR payments (2%)",
            SalesFilter {
                tender: Some(TenderKind::QrExternal),
                ..SalesFilter::default()
            },
        ),
        (
            "QR payments in January",
            SalesFilter {
                from: day("2025-01-01"),
                to: day("2025-01-31"),
                tender: Some(TenderKind::QrExternal),
                ..SalesFilter::default()
            },
        ),
    ];
    let budget = Duration::from_millis(300);
    let mut worst = Duration::ZERO;
    for (name, filter) in cases.drain(..) {
        // Warm once (the first query after ANALYZE plans and fills the page cache), then time.
        t.sales.list(owner, &filter, None, None).await.unwrap();
        let started = Instant::now();
        let page = t.sales.list(owner, &filter, None, None).await.unwrap();
        let took = started.elapsed();
        eprintln!("{name}: {} rows in {took:?}", page.rows.len());
        assert!(!page.rows.is_empty(), "{name}");
        assert!(took < budget, "{name}: {took:?}");
        worst = worst.max(took);
    }
    // Page 200 (rows 9,951..10,000), following the tokens.
    let mut token: Option<String> = None;
    let mut last = Duration::ZERO;
    for _ in 0..200 {
        let started = Instant::now();
        let page = t
            .sales
            .list(owner, &SalesFilter::default(), token.as_deref(), None)
            .await
            .unwrap();
        last = started.elapsed();
        token = page.next.map(|c| c.encode());
    }
    eprintln!("page 200: {last:?}");
    assert!(last < budget, "page 200: {last:?}");
    eprintln!("worst first page: {worst:?}");
    // A detail of a sale in the middle.
    let mid = t
        .sales
        .list(
            owner,
            &SalesFilter {
                receipt: Some("62000".into()),
                ..SalesFilter::default()
            },
            None,
            None,
        )
        .await
        .unwrap();
    let started = Instant::now();
    let d = t.sales.get(owner, mid.rows[0].sale_id).await.unwrap();
    eprintln!("detail: {:?}", started.elapsed());
    assert_eq!(d.lines.len(), 4);
    assert!(started.elapsed() < budget);
}
