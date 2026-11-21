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

/// E15-T02: a Z summary. Float 100.00; 12 sales, cash net 80.00 + card 43.45; two returns
/// refunded 2.50 cash and 4.00 to a card; counted 177.00 against 177.50 expected.
fn z_doc() -> ReceiptDocument {
    use pos_domain::cash::{ClosedSession, SessionTotals, TenderTotal};
    let g = ids();
    let tt = |kind, count, minor| TenderTotal {
        kind,
        count,
        amount: Money::from_minor(minor),
    };
    let totals = SessionTotals {
        sales_count: 12,
        sales_total: Money::from_minor(12_345),
        tax_total: Money::from_minor(2_057),
        discount_total: Money::from_minor(300),
        rounding_total: Money::from_minor(2),
        tenders: vec![
            tt(TenderKind::CardExternal, 4, 4_345),
            tt(TenderKind::Cash, 9, 8_000),
        ],
        returns_count: 2,
        refunds_total: Money::from_minor(650),
        refunds: vec![tt(TenderKind::CardExternal, 1, 400), tt(TenderKind::Cash, 1, 250)],
    };
    let float = Money::from_minor(10_000);
    let expected = totals.expected_cash(float).unwrap();
    assert_eq!(expected.minor(), 17_750);
    let counted = Money::from_minor(17_700);
    let closed = ClosedSession {
        id: pos_domain::CashSessionId::generate(&g),
        closed_by: UserId::generate(&g),
        closed_at: Timestamp::from_millis(1_767_288_600_000),
        opening_float: float,
        counted,
        expected,
        variance: pos_domain::cash::variance(counted, expected).unwrap(),
        totals,
        note: Some("50p coin found under the drawer after the count".into()),
    };
    let mut ctx = context();
    ctx.cashier_name = "Mia".into();
    pos_receipts::z_summary(
        &closed,
        &ctx,
        &pos_receipts::ZContext {
            session_number: "R01-Z-0007".into(),
            opened_local: "2026-01-01 08:58".into(),
            opened_by: "Olu".into(),
            closed_local: "2026-01-01 17:30".into(),
            currency_exponent: 2,
            sandbox: false,
        },
    )
}

#[test]
fn golden_z_summary() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    let doc = z_doc();
    check(
        dir.join("z_summary.json"),
        &serde_json::to_string_pretty(&doc).unwrap(),
    );
    for cols in [COLUMNS_58MM, COLUMNS_80MM] {
        let lines = render(&doc, cols);
        assert!(lines.iter().all(|l| l.text.chars().count() <= cols));
        check(dir.join(format!("z_summary.{cols}.txt")), &to_plain_text(&lines));
        check_bytes(dir.join(format!("z_summary.{cols}.bin")), &to_escpos(&lines));
    }
    // Stored and read back, it renders the same.
    let back: ReceiptDocument = serde_json::from_str(&serde_json::to_string(&doc).unwrap()).unwrap();
    assert_eq!(render(&back, COLUMNS_80MM), render(&doc, COLUMNS_80MM));
}

/// D-02 (Sri Lanka): a tax invoice for a VAT-registered buyer, Gazette 2481/22. Two VAT lines
/// at 18% (prices exclusive), one exempt line left off and counted.
fn tax_invoice_doc() -> ReceiptDocument {
    use pos_receipts::{Meta, TaxInvoiceLine, TaxInvoiceSection};
    let mut ctx = context();
    ctx.business_name = "Lanka Traders (Pvt) Ltd".into();
    ctx.address_lines = vec!["12 Galle Road".into(), "Colombo 03".into()];
    ctx.phone = Some("011 234 5678".into());
    let line = |description: &str, quantity: &str, unit: i64, amount: i64| TaxInvoiceLine {
        description: description.into(),
        quantity: quantity.into(),
        unit_price: Money::from_minor(unit),
        amount: Money::from_minor(amount),
    };
    pos_receipts::tax_invoice(
        &ctx,
        Meta {
            receipt_number: "26NOV_R01_00001".into(),
            local_time: "2026-11-20 10:15".into(),
            register_name: "Till 1".into(),
            register_code: "R01".into(),
            cashier_name: "Nimal".into(),
        },
        false,
        2,
        TaxInvoiceSection {
            supplier_tin: "104567890".into(),
            purchaser_tin: "114223344".into(),
            purchaser_name: "Ceylon Office Supplies (Pvt) Ltd".into(),
            purchaser_address: vec!["45 Duplication Road".into(), "Colombo 04".into()],
            invoice_date: "11/20/2026".into(),
            supply_date: "11/20/2026".into(),
            sale_receipt: "R01-000042".into(),
            lines: vec![
                line(
                    "A4 copy paper, 80 gsm, ream of 500 sheets",
                    "10",
                    185_000,
                    1_850_000,
                ),
                line("Ballpoint pen, blue (box of 50)", "2", 120_000, 240_000),
            ],
            value_of_supply: Money::from_minor(2_090_000),
            vat: vec![pos_receipts::document::TaxRow {
                name: "VAT 18%".into(),
                amount: Money::from_minor(376_200),
            }],
            total: Money::from_minor(2_466_200),
            payment: vec!["Bank transfer".into()],
            omitted_lines: 1,
        },
    )
}

#[test]
fn golden_tax_invoice() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    let doc = tax_invoice_doc();
    check(
        dir.join("tax_invoice.json"),
        &serde_json::to_string_pretty(&doc).unwrap(),
    );
    for cols in [COLUMNS_58MM, COLUMNS_80MM] {
        let lines = render(&doc, cols);
        assert!(lines.iter().all(|l| l.text.chars().count() <= cols));
        let text = to_plain_text(&lines);
        // The gazette's particulars, in order.
        let order = [
            "TAX INVOICE",
            "Supplier TIN: 104567890",
            "Purchaser TIN: 114223344",
            "26NOV_R01_00001",
            "11/20/2026",
            "Total value of supply",
            "VAT 18%",
            "TOTAL INCL. VAT",
        ];
        let at: Vec<usize> = order
            .iter()
            .map(|s| text.find(s).unwrap_or_else(|| panic!("{s} missing")))
            .collect();
        assert!(
            at.windows(2).all(|w| w[0] < w[1]),
            "out of order at {cols} columns"
        );
        check(dir.join(format!("tax_invoice.{cols}.txt")), &text);
        check_bytes(dir.join(format!("tax_invoice.{cols}.bin")), &to_escpos(&lines));
    }
    // Value of supply + VAT = total, and stored then read back it renders the same.
    let ti = doc.tax_invoice.as_ref().unwrap();
    let vat: i64 = ti.vat.iter().map(|r| r.amount.minor()).sum();
    assert_eq!(ti.value_of_supply.minor() + vat, ti.total.minor());
    let back: ReceiptDocument = serde_json::from_str(&serde_json::to_string(&doc).unwrap()).unwrap();
    assert_eq!(render(&back, COLUMNS_80MM), render(&doc, COLUMNS_80MM));
    // The A4 PDF renders.
    let pdf = pos_receipts::pdf::render_pdf(&doc, pos_receipts::pdf::PdfFormat::A4).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
}

// ---------------------------------------------------------------------------------------
// E13-T05: code pages, transliteration, dialects, barcodes, other paper widths.

use pos_receipts::printer::{Op, decode, parse};
use pos_receipts::text::{Style, TextLine, text_columns};
use pos_receipts::{CodePage, CutMode, Dialect, DrawerPin, EncodeOptions, encode};

/// Names in Latin with accents, Polish, Greek, Russian, Japanese and symbols that only some
/// code pages have.
fn intl_doc() -> ReceiptDocument {
    let g = ids();
    let vat = rate(&g, "VAT 20%", 200_000);
    let mut c = Cart::new(&g);
    for (name, label, price, qty) in [
        ("Café crème 250 g — Größe L", "", 450, 1),
        ("Żubrówka 0,5 l", "", 1899, 1),
        ("Ελληνικός καφές", "", 320, 2),
        ("Квас хлебный", "", 210, 1),
        ("抹茶ラテ", "", 395, 1),
        ("Jalapeño ½ kg £ € ¥", "", 125, 1),
        ("Ærøskøbing Øl", "33 cl", 289, 3),
    ] {
        c.add(
            item(&g, name, label, price, Unit::Ea, vec![vat.clone()]),
            Quantity::from_whole(qty).unwrap(),
            AddVia::Search,
            &g,
        )
        .unwrap();
    }
    let mut ctx = context();
    ctx.business_name = "Café Zoë — Łódź".into();
    ctx.address_lines = vec!["ul. Piotrkowska 1".into(), "90-001 Łódź".into()];
    ctx.footer_lines = vec![
        "Merci — à bientôt!".into(),
        "Dziękujemy · Спасибо · Ευχαριστώ".into(),
        "ありがとう 谢谢".into(),
    ];
    ctx.cashier_name = "Zoë".into();
    sale_receipt(
        &sale(&c, TaxMode::Inclusive, &[cash(10_000)], SaleMode::Live),
        &ctx,
        "R01-000042",
        CopyKind::Original,
    )
}

/// An Epson TM-T88-style profile: the escpos-printer-db numbers for each page.
fn epson_pages() -> Vec<(CodePage, u8)> {
    vec![
        (CodePage::Cp858, 19),
        (CodePage::Cp852, 18),
        (CodePage::Cp866, 17),
        (CodePage::Cp737, 14),
        (CodePage::Cp1252, 16),
        (CodePage::JisX0201, 1),
    ]
}

/// A Star TSP100-style profile: Star Line Mode `ESC GS t n` numbers.
fn star_pages() -> Vec<(CodePage, u8)> {
    vec![
        (CodePage::Cp858, 4),
        (CodePage::Cp852, 5),
        (CodePage::Cp866, 10),
        (CodePage::Cp737, 15),
        (CodePage::Cp1252, 32),
    ]
}

/// Golden bytes plus their decode (`.txt`) for review.
fn check_job(dir: &std::path::Path, stem: &str, bytes: &[u8], opts: &EncodeOptions) {
    check(
        dir.join(format!("{stem}.txt")),
        &decode(bytes, opts.dialect, &opts.code_pages),
    );
    check_bytes(dir.join(format!("{stem}.bin")), bytes);
}

#[test]
fn golden_international_receipt_in_every_dialect() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    let doc = intl_doc();
    check(
        dir.join("intl.json"),
        &(serde_json::to_string_pretty(&doc).unwrap() + "\n"),
    );
    for width in [COLUMNS_58MM, 42, COLUMNS_80MM, 56, 64] {
        let lines = render(&doc, width);
        assert!(lines.iter().all(|l| text_columns(&l.text) <= width));
        check(dir.join(format!("intl.{width}.txt")), &to_plain_text(&lines));
    }

    // Today's default: ASCII with transliteration, nothing else changes.
    let lines = render(&doc, COLUMNS_58MM);
    let opts = EncodeOptions::default();
    check_job(&dir, "intl.32.escpos-ascii", &to_escpos(&lines), &opts);

    let lines = render(&doc, COLUMNS_80MM);
    let opts = EncodeOptions {
        code_pages: epson_pages(),
        cut: CutMode::Full,
        barcode: true,
        drawer_kick: Some(DrawerPin::Pin2),
        ..EncodeOptions::default()
    };
    check_job(&dir, "intl.48.escpos", &encode(&lines, &opts), &opts);

    let opts = EncodeOptions {
        dialect: Dialect::StarLine,
        code_pages: star_pages(),
        qr: true,
        drawer_kick: Some(DrawerPin::Pin5),
        ..EncodeOptions::default()
    };
    check_job(&dir, "intl.48.star", &encode(&lines, &opts), &opts);

    let lines = render(&doc, COLUMNS_58MM);
    let opts = EncodeOptions {
        dialect: Dialect::PlainText,
        code_pages: vec![(CodePage::Cp1252, 0)],
        cut: CutMode::Full,
        barcode: true,
        ..EncodeOptions::default()
    };
    check_job(&dir, "intl.32.plain", &encode(&lines, &opts), &opts);
}

#[test]
fn golden_barcode_on_a_return_and_qr_on_a_z_summary() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    let ret = scenarios().into_iter().find(|(n, _)| *n == "return").unwrap().1;
    let opts = EncodeOptions {
        code_pages: epson_pages(),
        barcode: true,
        ..EncodeOptions::default()
    };
    check_job(&dir, "return.42.escpos", &encode(&render(&ret, 42), &opts), &opts);
    let opts = EncodeOptions {
        qr: true,
        cut: CutMode::None,
        feed_before_cut: 5,
        ..EncodeOptions::default()
    };
    check_job(
        &dir,
        "z_summary.32.escpos-qr",
        &encode(&render(&z_doc(), 32), &opts),
        &opts,
    );
}

/// The text of each printed line, one byte per column.
fn printed_lines(bytes: &[u8], dialect: Dialect) -> Vec<Vec<u8>> {
    let mut lines = vec![Vec::new()];
    for op in parse(bytes, dialect) {
        match op {
            Op::Text(t) => lines.last_mut().unwrap().extend(t),
            Op::Lf => lines.push(Vec::new()),
            Op::Unknown(b) => panic!("unknown byte {b:#04X}"),
            _ => {}
        }
    }
    lines
}

#[test]
fn every_encoded_line_stays_within_its_columns() {
    let mut docs: Vec<(&str, ReceiptDocument)> = scenarios();
    docs.push(("intl", intl_doc()));
    docs.push(("z_summary", z_doc()));
    let option_sets = [
        EncodeOptions::default(),
        EncodeOptions {
            code_pages: epson_pages(),
            ..EncodeOptions::default()
        },
        EncodeOptions {
            code_pages: vec![(CodePage::Cp437, 0)],
            ..EncodeOptions::default()
        },
        EncodeOptions {
            dialect: Dialect::StarLine,
            code_pages: star_pages(),
            ..EncodeOptions::default()
        },
        EncodeOptions {
            dialect: Dialect::PlainText,
            code_pages: vec![(CodePage::Cp1252, 0)],
            ..EncodeOptions::default()
        },
        EncodeOptions {
            dialect: Dialect::PlainText,
            ..EncodeOptions::default()
        },
    ];
    for (name, doc) in &docs {
        for width in [COLUMNS_58MM, 42, COLUMNS_80MM, 56, 64] {
            let lines = render(doc, width);
            for opts in &option_sets {
                let printed = printed_lines(&encode(&lines, opts), opts.dialect);
                assert!(printed.len() > lines.len(), "{name}");
                for (l, p) in lines.iter().zip(&printed) {
                    let laid_out = text_columns(&l.text);
                    assert!(
                        p.len() <= width,
                        "{name} at {width} ({:?}): {:?} prints {} columns",
                        opts.dialect,
                        l.text,
                        p.len()
                    );
                    if laid_out == width {
                        // Full lines (rules, amounts) keep their right edge.
                        assert_eq!(p.len(), width, "{name} at {width}: {:?}", l.text);
                    }
                }
            }
        }
    }
}

#[test]
fn non_latin_text_prints_natively_where_a_page_has_it() {
    let lines = render(&intl_doc(), COLUMNS_80MM);
    let opts = EncodeOptions {
        code_pages: epson_pages(),
        ..EncodeOptions::default()
    };
    let decoded = decode(&encode(&lines, &opts), Dialect::EscPos, &opts.code_pages);
    for want in [
        "Café crème 250 g — Größe L",
        "Żubrówka 0,5 l",
        "Ελληνικός καφές",
        "Квас хлебный",
        "Ærøskøbing Øl 33 cl",
        "Dziękujemy",
        "Спасибо",
        "Ευχαριστώ",
    ] {
        assert!(decoded.contains(want), "{want} missing from:\n{decoded}");
    }
    // With no code pages the same receipt is readable ASCII, never `?` for Latin letters.
    let ascii = String::from_utf8(to_escpos(&lines)).unwrap_or_default();
    for want in ["Cafe creme 250 g - Grosse L", "Zubrowka 0,5 l", "Kvas khlebnyi"] {
        assert!(ascii.contains(want), "{want} missing from:\n{ascii}");
    }
}

#[test]
fn receipts_lay_out_at_42_56_and_64_columns() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    let mut docs: Vec<(&str, ReceiptDocument)> = scenarios();
    docs.push(("z_summary", z_doc()));
    for (name, doc) in &docs {
        for width in [42, 56, 64] {
            let lines = render(doc, width);
            assert!(
                lines.iter().all(|l| text_columns(&l.text) <= width),
                "{name} overflows {width}"
            );
            assert!(
                lines.iter().any(|l| l.text == "-".repeat(width)),
                "{name} at {width}: no full-width rule"
            );
            // Totals are right-aligned on the last column.
            let total = lines
                .iter()
                .find(|l| l.style == Style::Bold)
                .unwrap_or_else(|| panic!("{name}: no bold total"));
            assert_eq!(text_columns(&total.text), width, "{name} at {width}: {total:?}");
            // The receipt number is the last line, centred, as one barcode line.
            let last: &TextLine = lines.last().unwrap();
            assert_eq!(last.style, Style::Barcode, "{name}");
            assert_eq!(last.text.trim(), doc.barcode);
            let pad = last.text.len() - last.text.trim_start().len();
            assert_eq!(pad, (width - doc.barcode.len()) / 2, "{name} at {width}");
        }
    }
    let sale = &docs.iter().find(|(n, _)| *n == "sale").unwrap().1;
    check(dir.join("sale.42.txt"), &to_plain_text(&render(sale, 42)));
    let discounts = &docs.iter().find(|(n, _)| *n == "discounts").unwrap().1;
    check(
        dir.join("discounts.56.txt"),
        &to_plain_text(&render(discounts, 56)),
    );
    check(
        dir.join("z_summary.64.txt"),
        &to_plain_text(&render(&z_doc(), 64)),
    );
}
