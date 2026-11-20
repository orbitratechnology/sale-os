//! D-02 (Sri Lanka): VAT tax invoices for registered buyers, against a real database.

mod common;

use pos_application::cart::CartService;
use pos_application::catalog::CatalogService;
use pos_application::checkout::{CheckoutService, CompleteSale};
use pos_application::scan::ScanSource;
use pos_application::tax_invoice::{Purchaser, TaxInvoiceService};
use pos_domain::catalog::{ProductInput, ProductType, Unit, VariantInput};
use pos_domain::settlement::{TenderInput, TenderKind};
use pos_domain::{ErrorCode, HasErrorCode, Money, Role, SaleId, ScanEventId, TaxCategoryId, UuidV7Generator};
use pos_local_db::SqliteStore;
use std::sync::Arc;

/// The shop (VAT 20% included in prices, 2026-01-01 in London) with a supplier TIN, a VAT
/// product (Tea 2.50) and an exempt one (Bread 1.00, a category with no rates).
async fn shop() -> common::Shop {
    let shop = common::shop().await;
    let s = &shop.store;
    sqlx::query("UPDATE businesses SET tax_registration_no = '104567890'")
        .execute(s.db().writer())
        .await
        .unwrap();
    let exempt = TaxCategoryId::generate(&UuidV7Generator);
    sqlx::query(
        "INSERT INTO tax_categories (tax_category_id, business_id, name, created_at, updated_at)
         SELECT ?, business_id, 'Exempt', 0, 0 FROM businesses",
    )
    .bind(exempt.to_string())
    .execute(s.db().writer())
    .await
    .unwrap();
    let catalog = CatalogService {
        store: Arc::new(s.clone()),
        catalog: Arc::new(s.clone()),
    };
    for (name, price, sku, tax) in [
        ("Tea", 250, "TEA", shop.tax_category),
        ("Bread", 100, "BREAD", exempt),
    ] {
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
                    tax_category_id: tax,
                    quick_select: false,
                    variants: vec![v],
                },
            )
            .await
            .unwrap();
    }
    shop
}

async fn sell(shop: &common::Shop, codes: &[&str]) -> SaleId {
    let s = shop.store.clone();
    let carts = Arc::new(CartService::new(
        Arc::new(s.clone()),
        Arc::new(s.clone()),
        Arc::new(s.clone()),
    ));
    for code in codes {
        carts
            .scan(
                &shop.owner,
                code,
                ScanEventId::generate(&UuidV7Generator),
                ScanSource::Manual,
            )
            .await
            .unwrap();
    }
    let cart = carts.current(&shop.owner).await.unwrap().cart;
    CheckoutService {
        carts,
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
    .unwrap()
    .sale
    .id
}

fn service(s: &SqliteStore) -> TaxInvoiceService {
    TaxInvoiceService {
        store: Arc::new(s.clone()),
        receipts: Arc::new(s.clone()),
        sales: Arc::new(s.clone()),
        pricing: Arc::new(s.clone()),
    }
}

fn buyer() -> Purchaser {
    Purchaser {
        tin: "114-223-344".into(),
        name: "Ceylon Office Supplies (Pvt) Ltd".into(),
        address: vec!["45 Duplication Road".into(), "  ".into(), "Colombo 04".into()],
    }
}

#[tokio::test]
async fn a_tax_invoice_states_the_vat_lines_excluding_vat_and_is_numbered_per_register() {
    let shop = shop().await;
    let invoices = service(&shop.store);
    // Two teas (5.00 including 0.83 VAT) and a loaf without VAT.
    let sale = sell(&shop, &["TEA", "TEA", "BREAD"]).await;
    assert!(invoices.get(&shop.owner, sale, None).await.unwrap().is_none());

    let issued = invoices.issue(&shop.owner, sale, buyer()).await.unwrap();
    let doc = &issued.document;
    assert_eq!(doc.meta.receipt_number, "26JAN_R01_00001");
    assert_eq!(doc.barcode, "26JAN_R01_00001");
    let ti = doc.tax_invoice.as_ref().unwrap();
    assert_eq!(
        (
            ti.supplier_tin.as_str(),
            ti.purchaser_tin.as_str(),
            ti.invoice_date.as_str(),
            ti.supply_date.as_str()
        ),
        ("104567890", "114223344", "01/01/2026", "01/01/2026")
    );
    assert_eq!(ti.purchaser_address, ["45 Duplication Road", "Colombo 04"]);
    assert_eq!(ti.sale_receipt, "R01-000001");
    // Only the VAT line, excluding VAT: 5.00 − 0.83 = 4.17 for 2 at 2.09 (rounded).
    assert_eq!(ti.lines.len(), 1);
    assert_eq!(
        (
            ti.lines[0].description.as_str(),
            ti.lines[0].quantity.as_str(),
            ti.lines[0].amount.minor(),
            ti.lines[0].unit_price.minor()
        ),
        ("Tea", "2", 417, 209)
    );
    assert_eq!(ti.omitted_lines, 1);
    let vat: i64 = ti.vat.iter().map(|r| r.amount.minor()).sum();
    assert_eq!(
        (ti.value_of_supply.minor(), vat, ti.total.minor()),
        (417, 83, 500)
    );
    assert_eq!(ti.payment, ["Cash"]);
    let text = pos_receipts::text::to_plain_text(&issued.lines);
    assert!(
        text.contains("TAX INVOICE") && text.contains("Purchaser TIN: 114223344"),
        "{text}"
    );

    // Stored: read back as issued, audited, and the sale's own receipt is unchanged.
    let again = invoices.get(&shop.owner, sale, Some(58)).await.unwrap().unwrap();
    assert_eq!(again.document, issued.document);
    let audited: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE action = 'tax_invoice.issued'")
            .fetch_one(shop.store.db().readers())
            .await
            .unwrap();
    assert_eq!(audited, 1);

    // One per sale; the next sale's invoice takes the next number.
    let e = invoices.issue(&shop.owner, sale, buyer()).await.unwrap_err();
    assert_eq!(e.error_code(), ErrorCode::ValidationFailed);
    assert!(e.to_string().contains("26JAN_R01_00001"), "{e}");
    let second = sell(&shop, &["TEA"]).await;
    let next = invoices.issue(&shop.owner, second, buyer()).await.unwrap();
    assert_eq!(next.document.meta.receipt_number, "26JAN_R01_00002");

    // Immutable, like the receipts.
    for sql in [
        "UPDATE tax_invoices SET purchaser_tin = '000000000'",
        "DELETE FROM tax_invoices",
    ] {
        assert!(
            sqlx::query(sql).execute(shop.store.db().writer()).await.is_err(),
            "{sql}"
        );
    }
}

#[tokio::test]
async fn refused_without_vat_tins_or_an_address_and_nothing_is_written() {
    let shop = shop().await;
    let invoices = service(&shop.store);
    let bread = sell(&shop, &["BREAD"]).await;
    let tea = sell(&shop, &["TEA"]).await;
    let refused = |what: &str, p: Purchaser, sale: SaleId| {
        let invoices = &invoices;
        let owner = &shop.owner;
        let what = what.to_owned();
        async move {
            let e = invoices.issue(owner, sale, p).await.unwrap_err();
            assert_eq!(e.error_code(), ErrorCode::ValidationFailed, "{what}: {e}");
            e.to_string()
        }
    };
    assert!(refused("no VAT", buyer(), bread).await.contains("no VAT"));
    let mut p = buyer();
    p.tin = "GB123456789".into();
    assert!(refused("TIN", p, tea).await.contains("9 digits"));
    let mut p = buyer();
    p.name = " ".into();
    refused("name", p, tea).await;
    let mut p = buyer();
    p.address = vec![];
    refused("address", p, tea).await;
    // Without the shop's own TIN nothing can be issued.
    sqlx::query("UPDATE businesses SET tax_registration_no = NULL")
        .execute(shop.store.db().writer())
        .await
        .unwrap();
    assert!(refused("supplier TIN", buyer(), tea).await.contains("Settings"));
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM tax_invoices")
        .fetch_one(shop.store.db().readers())
        .await
        .unwrap();
    assert_eq!(n, 0);
    // A cashier may issue one (it is part of selling); viewing needs only sales access.
    sqlx::query("UPDATE businesses SET tax_registration_no = '104567890'")
        .execute(shop.store.db().writer())
        .await
        .unwrap();
    invoices
        .issue(&shop.as_role(Role::Cashier), tea, buyer())
        .await
        .unwrap();
}
