//! E14-T04: CSV exports against a real database.

mod common;

use pos_application::cart::CartService;
use pos_application::catalog::CatalogService;
use pos_application::checkout::{CheckoutService, CompleteSale};
use pos_application::export::{Dataset, ExportService};
use pos_application::returns::{CreateReturn, ReturnService};
use pos_application::scan::ScanSource;
use pos_domain::catalog::{BarcodeInput, ProductInput, ProductType, Unit, VariantInput};
use pos_domain::returns::{Disposition, ReturnLineRequest, ReturnReason};
use pos_domain::settlement::{TenderInput, TenderKind};
use pos_domain::time::BusinessDate;
use pos_domain::{ErrorCode, HasErrorCode, Money, Quantity, Role, ScanEventId, UuidV7Generator};
use std::sync::Arc;

fn text(bytes: &[u8]) -> String {
    let s = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(s.starts_with('\u{feff}'), "UTF-8 BOM");
    assert!(s.ends_with("\r\n"));
    s.trim_start_matches('\u{feff}').to_owned()
}

#[tokio::test]
async fn exports_are_exact_quoted_and_audited() {
    let shop = common::shop().await;
    let s = shop.store.clone();
    let owner = &shop.owner;
    let catalog = CatalogService {
        store: Arc::new(s.clone()),
        catalog: Arc::new(s.clone()),
    };
    for (name, price, sku, unit) in [
        ("Tea, green", 250, "TEA", Unit::Ea),
        ("Flour", 180, "FLOUR", Unit::Kg),
    ] {
        let mut v = VariantInput::simple(Money::from_minor(price));
        v.sku = Some(sku.into());
        v.cost = Some(Money::from_minor(price / 2));
        if sku == "TEA" {
            v.barcodes = vec![BarcodeInput::alias("4006381333931")];
            v.low_stock_threshold = Some(Quantity::from_milli(5_000));
        }
        catalog
            .create(
                owner,
                ProductInput {
                    name: name.into(),
                    description: None,
                    category_id: None,
                    product_type: ProductType::Stock,
                    unit,
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
    for code in ["TEA", "TEA"] {
        carts
            .scan(
                owner,
                code,
                ScanEventId::generate(&UuidV7Generator),
                ScanSource::Manual,
            )
            .await
            .unwrap();
    }
    let cart = carts.current(owner).await.unwrap().cart;
    CheckoutService {
        carts: carts.clone(),
        store: Arc::new(s.clone()),
        setup: Arc::new(s.clone()),
        stock: Arc::new(s.clone()),
        receipts: Arc::new(s.clone()),
    }
    .complete_sale(
        owner,
        CompleteSale {
            cart_id: cart.id,
            revision: cart.revision,
            tenders: vec![
                TenderInput {
                    kind: TenderKind::CardExternal,
                    amount: Money::from_minor(300),
                    external_reference: Some("AUTH, 7".into()),
                },
                TenderInput {
                    kind: TenderKind::Cash,
                    amount: Money::from_minor(500),
                    external_reference: None,
                },
            ],
        },
    )
    .await
    .unwrap();
    let returns = ReturnService {
        store: Arc::new(s.clone()),
        returns: Arc::new(s.clone()),
        setup: Arc::new(s.clone()),
        receipts: Arc::new(s.clone()),
        pricing: Arc::new(s.clone()),
    };
    let sale = returns.find(owner, "R01-000001").await.unwrap();
    returns
        .create(
            owner,
            CreateReturn {
                sale_id: sale.sale_id,
                lines: vec![ReturnLineRequest {
                    sale_line_id: sale.lines[0].sale_line_id,
                    quantity: Quantity::from_milli(1000),
                    disposition: Disposition::Restock,
                }],
                reason: ReturnReason::Defective,
                note: None,
                refunds: vec![TenderInput {
                    kind: TenderKind::Cash,
                    amount: Money::from_minor(250),
                    external_reference: None,
                }],
            },
        )
        .await
        .unwrap();

    let exports = ExportService {
        store: Arc::new(s.clone()),
        exports: Arc::new(s.clone()),
        reports: Arc::new(s.clone()),
    };
    let day: BusinessDate = "2026-01-01".parse().unwrap();
    let range = Some((day, day));

    let t = exports.build(owner, Dataset::Transactions, range).await.unwrap();
    assert_eq!(t.file_name, "transactions_2026-01-01_2026-01-01.csv");
    let csv = text(&t.bytes);
    let lines: Vec<&str> = csv.split("\r\n").filter(|l| !l.is_empty()).collect();
    assert_eq!(
        lines[0],
        "receipt_number,business_date,local_time,mode,cashier,line_no,name,variant,sku,barcode,unit,quantity,unit_price,price_overridden,gross,line_discount,order_discount_share,tax,line_total"
    );
    assert_eq!(
        lines[1],
        "R01-000001,2026-01-01,2026-01-01 00:00,sandbox,Olu,1,\"Tea, green\",,TEA,,ea,2,2.50,no,5.00,0.00,0.00,0.83,5.00"
    );
    assert_eq!(t.rows, 1);

    let csv = text(&exports.build(owner, Dataset::Tenders, range).await.unwrap().bytes);
    assert!(
        csv.contains(
            "R01-000001,2026-01-01,sandbox,1,card_external,3.00,,0.00,confirmed,operator,\"AUTH, 7\"\r\n"
        ),
        "{csv}"
    );
    assert!(
        csv.contains("R01-000001,2026-01-01,sandbox,2,cash,2.00,5.00,3.00,confirmed,operator,\r\n"),
        "{csv}"
    );

    let csv = text(&exports.build(owner, Dataset::Returns, range).await.unwrap().bytes);
    assert!(
        csv.contains("R01-R-000001,2026-01-01,2026-01-01 00:00,sandbox,R01-000001,defective,\"Tea, green\",,1,restock,2.50,0.42,cash\r\n"),
        "{csv}"
    );

    let products = exports.build(owner, Dataset::Products, None).await.unwrap();
    let csv = text(&products.bytes);
    assert!(csv.starts_with("name,variant,sku,barcode,price,cost,unit,tax_category,track_stock,low_stock_threshold,scan_quantity,status\r\n"));
    assert!(csv.contains("Flour,,FLOUR,,1.80,0.90,kg,"), "{csv}");
    assert!(
        csv.contains("\"Tea, green\",,TEA,4006381333931,2.50,1.25,ea,"),
        "{csv}"
    );
    assert!(csv.contains(",yes,5,1,active\r\n"), "{csv}");

    let csv = text(&exports.build(owner, Dataset::Stock, None).await.unwrap().bytes);
    assert!(
        csv.contains("\"Tea, green\",,TEA,ea,-1,0,5\r\n"),
        "sold 2, 1 restocked: {csv}"
    );

    // Report exports are live only: this practice day has none.
    let summary = exports.build(owner, Dataset::SalesSummary, range).await.unwrap();
    assert_eq!(summary.rows, 0);
    assert!(text(&summary.bytes).starts_with("business_date,sales,gross,"));

    // A range is needed for dated datasets; permissions follow the dataset.
    assert!(exports.build(owner, Dataset::Transactions, None).await.is_err());
    let cashier = shop.as_role(Role::Cashier);
    for d in [Dataset::Transactions, Dataset::Products] {
        assert_eq!(
            exports.build(&cashier, d, range).await.unwrap_err().error_code(),
            ErrorCode::PermissionDenied
        );
    }
    let manager = shop.as_role(Role::Manager);
    assert!(exports.build(&manager, Dataset::Products, None).await.is_ok());

    // Recorded once saved.
    exports.record(owner, &t).await.unwrap();
    let audited: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE action = 'report.exported' AND json_extract(metadata_json, '$.dataset') = 'transactions' AND json_extract(metadata_json, '$.rows') = 1",
    )
    .fetch_one(s.db().readers())
    .await
    .unwrap();
    assert_eq!(audited, 1);
}
