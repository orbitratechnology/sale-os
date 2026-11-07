//! E13-T02/T03 golden receipts: the JSON document and its 32- and 48-column text for a set
//! of hand-checked sales. Regenerate with `UPDATE_GOLDEN=1 cargo test -p pos-receipts`, then
//! review the diff.

use pos_domain::cart::{AddVia, Cart, SellableVariant};
use pos_domain::catalog::{ProductType, Unit};
use pos_domain::pricing::{Discount, PricingPolicy, TaxMode, TaxRateSnapshot, price_cart};
use pos_domain::sale::{SaleHeader, SaleMode, SaleSnapshot};
use pos_domain::settlement::{TenderInput, TenderKind, settle};
use pos_domain::{
    Currency, CurrencyCode, IdGenerator, Money, ProductId, Quantity, RatePpm, TaxRateId, Timestamp, UserId,
    VariantId,
};
use pos_receipts::text::{COLUMNS_58MM, COLUMNS_80MM, render, to_escpos, to_plain_text};
use pos_receipts::{CopyKind, ReceiptContext, ReceiptDocument, sale_receipt};
use pos_testkit::SeqIdGenerator;
use std::path::PathBuf;

fn ids() -> SeqIdGenerator {
    SeqIdGenerator::new(1_767_225_600_000)
}

fn rate(ids: &dyn IdGenerator, name: &str, ppm: u32) -> TaxRateSnapshot {
    TaxRateSnapshot {
        id: TaxRateId::generate(ids),
        name: name.into(),
        rate: RatePpm::new(ppm).unwrap(),
    }
}

fn item(
    ids: &dyn IdGenerator,
    name: &str,
    label: &str,
    price: i64,
    unit: Unit,
    taxes: Vec<TaxRateSnapshot>,
) -> SellableVariant {
    SellableVariant {
        variant_id: VariantId::generate(ids),
        product_id: ProductId::generate(ids),
        name: name.into(),
        label: label.into(),
        sku: None,
        unit,
        price: Money::from_minor(price),
        taxes,
        track_stock: true,
        product_type: ProductType::Stock,
        cost: None,
    }
}

fn context() -> ReceiptContext {
    ReceiptContext {
        business_name: "Corner Shop".into(),
        address_lines: vec!["1 High Street".into(), "Leeds LS1 1AA".into()],
        phone: Some("0113 496 0000".into()),
        tax_registration_no: Some("GB123456789".into()),
        header_lines: vec![],
        footer_lines: vec![
            "Thank you!".into(),
            "Returns within 30 days with this receipt.".into(),
        ],
        register_name: "Till 1".into(),
        register_code: "R01".into(),
        cashier_name: "Olu".into(),
    }
}

/// 2026-11-07 14:03 in London (GMT).
fn header(mode: SaleMode, inclusive: bool) -> SaleHeader {
    SaleHeader {
        mode,
        currency: Currency::new(CurrencyCode::new("GBP").unwrap(), 2).unwrap(),
        prices_include_tax: inclusive,
        cashier: UserId::generate(&ids()),
        occurred_at: Timestamp::from_millis(1_794_060_180_000),
        business_date: "2026-11-07".parse().unwrap(),
        timezone: "Europe/London".into(),
    }
}

fn cash(minor: i64) -> TenderInput {
    TenderInput {
        kind: TenderKind::Cash,
        amount: Money::from_minor(minor),
        external_reference: None,
    }
}

fn sale(cart: &Cart, mode: TaxMode, tenders: &[TenderInput], sale_mode: SaleMode) -> SaleSnapshot {
    let policy = PricingPolicy::new(mode);
    let priced = price_cart(&cart.pricing_lines(), cart.order_discount.as_ref(), &policy).unwrap();
    let settlement = settle(priced.total, tenders, &policy).unwrap();
    SaleSnapshot::build(
        cart,
        &priced,
        settlement,
        header(sale_mode, mode == TaxMode::Inclusive),
        &ids(),
    )
    .unwrap()
}

fn scenarios() -> Vec<(&'static str, ReceiptDocument)> {
    let g = ids();
    let vat = rate(&g, "VAT 20%", 200_000);
    let mut out = Vec::new();

    // 2 × Tea 2.50, VAT included; cash 10.00 → change 5.00.
    let mut c = Cart::new(&g);
    c.add(
        item(&g, "Tea", "", 250, Unit::Ea, vec![vat.clone()]),
        Quantity::from_whole(2).unwrap(),
        AddVia::Search,
        &g,
    )
    .unwrap();
    out.push((
        "sale",
        sale_receipt(
            &sale(&c, TaxMode::Inclusive, &[cash(1000)], SaleMode::Live),
            &context(),
            "R01-000001",
            CopyKind::Original,
        ),
    ));

    // Split: card 5.00 recorded by the operator + cash 2.00 on 6.00.
    let mut c = Cart::new(&g);
    c.add(
        item(&g, "T-shirt", "M / Blue", 600, Unit::Ea, vec![vat.clone()]),
        Quantity::ONE,
        AddVia::Search,
        &g,
    )
    .unwrap();
    let card = TenderInput {
        kind: TenderKind::CardExternal,
        amount: Money::from_minor(500),
        external_reference: Some("AUTH 0042".into()),
    };
    out.push((
        "split",
        sale_receipt(
            &sale(&c, TaxMode::Inclusive, &[card, cash(200)], SaleMode::Live),
            &context(),
            "R01-000002",
            CopyKind::Original,
        ),
    ));

    // A 10% line discount and a 1.00 order discount; a price override.
    let mut c = Cart::new(&g);
    c.add(
        item(&g, "Kettle", "", 2000, Unit::Ea, vec![vat.clone()]),
        Quantity::ONE,
        AddVia::Search,
        &g,
    )
    .unwrap();
    c.add(
        item(&g, "Mug", "", 800, Unit::Ea, vec![vat.clone()]),
        Quantity::from_whole(2).unwrap(),
        AddVia::Search,
        &g,
    )
    .unwrap();
    let kettle = c.lines[0].id;
    let mug = c.lines[1].id;
    c.set_line_discount(kettle, Some(Discount::percent(RatePpm::new(100_000).unwrap())))
        .unwrap();
    c.override_price(mug, Money::from_minor(700)).unwrap();
    c.set_order_discount(Some(Discount::fixed(Money::from_minor(100))))
        .unwrap();
    out.push((
        "discounts",
        sale_receipt(
            &sale(&c, TaxMode::Inclusive, &[cash(5000)], SaleMode::Live),
            &context(),
            "R01-000003",
            CopyKind::Original,
        ),
    ));

    // Tax added at the till, two rates on one line, a weighed item: 0.333 kg × 12.49/kg.
    let state = rate(&g, "State 6%", 60_000);
    let city = rate(&g, "City 2%", 20_000);
    let mut c = Cart::new(&g);
    c.add(
        item(&g, "Cheese", "", 1249, Unit::Kg, vec![state, city]),
        Quantity::parse_decimal("0.333").unwrap(),
        AddVia::Search,
        &g,
    )
    .unwrap();
    c.add(
        item(&g, "Bread", "", 300, Unit::Ea, vec![]),
        Quantity::ONE,
        AddVia::Search,
        &g,
    )
    .unwrap();
    let s = sale(&c, TaxMode::Exclusive, &[cash(1000)], SaleMode::Live);
    out.push((
        "multi_rate",
        sale_receipt(&s, &context(), "R01-000004", CopyKind::Original),
    ));

    // A practice sale, printed again.
    let mut c = Cart::new(&g);
    c.add(
        item(
            &g,
            "A product with a very long name that has to wrap on narrow paper",
            "",
            199,
            Unit::Ea,
            vec![vat],
        ),
        Quantity::ONE,
        AddVia::Search,
        &g,
    )
    .unwrap();
    let doc = sale_receipt(
        &sale(&c, TaxMode::Inclusive, &[cash(199)], SaleMode::Sandbox),
        &context(),
        "R01-000005",
        CopyKind::Original,
    );
    out.push(("sandbox_reprint", doc.as_reprint()));

    // A return of 1 of 3 teas (3 × 3.33) with the cumulative rule, refunded in cash.
    let tea = item(&g, "Tea", "", 333, Unit::Ea, vec![rate(&g, "VAT 20%", 200_000)]);
    let mut c = Cart::new(&g);
    c.add(tea, Quantity::from_whole(3).unwrap(), AddVia::Search, &g)
        .unwrap();
    let sold = sale(&c, TaxMode::Inclusive, &[cash(999)], SaleMode::Live);
    out.push(("return", return_doc(&sold, &g)));
    out
}

fn return_doc(sold: &SaleSnapshot, g: &dyn IdGenerator) -> ReceiptDocument {
    use pos_domain::returns::{
        Disposition, RefundSnapshot, ReturnLineRequest, ReturnLineSnapshot, ReturnReason, ReturnSnapshot,
        ReturnableLine, ReturnableSale, plan_return, settle_refunds,
    };
    let l = &sold.lines[0];
    let returnable = ReturnableSale {
        sale_id: sold.id,
        receipt_number: "R01-000001".into(),
        mode: SaleMode::Live,
        currency: sold.currency,
        lines: vec![ReturnableLine {
            sale_line_id: l.id,
            line_no: 1,
            product_id: l.product_id,
            variant_id: l.variant_id,
            name: l.name.clone(),
            variant_label: l.variant_label.clone(),
            unit: l.unit,
            track_stock: true,
            quantity: l.quantity,
            total: l.total,
            tax: l.tax,
            unit_cost: None,
            returned: Quantity::ZERO,
        }],
    };
    let plan = plan_return(
        &returnable,
        SaleMode::Live,
        &[ReturnLineRequest {
            sale_line_id: l.id,
            quantity: Quantity::ONE,
            disposition: Disposition::Restock,
        }],
    )
    .unwrap();
    let refunds = settle_refunds(plan.total, &[cash(plan.total.minor())]).unwrap();
    let ret = ReturnSnapshot {
        id: pos_domain::ReturnId::generate(g),
        original_sale: sold.id,
        original_receipt_number: "R01-000001".into(),
        reason: ReturnReason::ChangedMind,
        note: None,
        currency: sold.currency,
        subtotal: plan.subtotal,
        tax: plan.tax,
        total: plan.total,
        lines: plan
            .lines
            .into_iter()
            .map(|line| ReturnLineSnapshot {
                id: pos_domain::ReturnLineId::generate(g),
                line,
            })
            .collect(),
        refunds: refunds
            .into_iter()
            .enumerate()
            .map(|(i, tender)| RefundSnapshot {
                id: pos_domain::RefundId::generate(g),
                seq: i as u32 + 1,
                tender,
            })
            .collect(),
        actor: UserId::generate(g),
        approver: None,
        occurred_at: sold.occurred_at,
        business_date: sold.business_date,
        timezone: sold.timezone.clone(),
    };
    pos_receipts::return_receipt(&ret, &context(), "R01-R-000001", false)
}

fn check(path: PathBuf, actual: &str) {
    if std::env::var("UPDATE_GOLDEN").is_ok() {
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "missing golden {}: run with UPDATE_GOLDEN=1 and review",
            path.display()
        )
    });
    assert_eq!(
        actual,
        expected,
        "{} differs (UPDATE_GOLDEN=1 to accept, then review the diff)",
        path.display()
    );
}

fn check_bytes(path: PathBuf, actual: &[u8]) {
    if std::env::var("UPDATE_GOLDEN").is_ok() {
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read(&path).unwrap_or_else(|_| panic!("missing golden {}", path.display()));
    if actual != expected.as_slice() {
        panic!(
            "{} differs; decoded actual:\n{}",
            path.display(),
            pos_receipts::escpos::decode(actual)
        );
    }
}

#[test]
fn golden_documents_and_text() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    for (name, doc) in scenarios() {
        check(
            dir.join(format!("{name}.json")),
            &(serde_json::to_string_pretty(&doc).unwrap() + "\n"),
        );
        for width in [COLUMNS_58MM, COLUMNS_80MM] {
            let lines = render(&doc, width);
            assert!(
                lines.iter().all(|l| l.text.chars().count() <= width),
                "{name} overflows {width} columns"
            );
            check(dir.join(format!("{name}.{width}.txt")), &to_plain_text(&lines));
            // AT-09: the exact ESC/POS bytes, reviewed through their decode.
            check_bytes(dir.join(format!("{name}.{width}.bin")), &to_escpos(&lines));
        }
        // The document round-trips through JSON (it is stored as JSON).
        let back: ReceiptDocument = serde_json::from_str(&serde_json::to_string(&doc).unwrap()).unwrap();
        assert_eq!(back, doc);
    }
}

#[test]
fn sums_on_the_receipt_match_the_sale() {
    for (name, doc) in scenarios() {
        let tendered: i64 = doc
            .tenders
            .iter()
            .map(|t| t.received.unwrap_or(t.amount).minor())
            .sum();
        assert_eq!(tendered - doc.change.minor(), doc.totals.total.minor(), "{name}");
        let net: i64 = doc
            .lines
            .iter()
            .map(|l| l.gross.minor() - l.discount.minor())
            .sum::<i64>()
            - doc.totals.order_discount.minor();
        let tax: i64 = if doc.totals.prices_include_tax {
            0
        } else {
            doc.totals.tax.iter().map(|t| t.amount.minor()).sum()
        };
        assert_eq!(
            net + tax + doc.totals.rounding.minor(),
            doc.totals.total.minor(),
            "{name}"
        );
    }
}
