//! E15-T03: the Stage 2 end-of-day scenario, scripted and fully offline with virtual devices
//! only, through the real services and database.
//!
//! The fixture day (prices include 20% VAT; live mode, so sales need an open drawer):
//!
//! | Sale | Items | Tenders | Total |
//! |---|---|---|---|
//! | S1 | 2 Cola | cash 5.00 (change 2.60) | 2.40 |
//! | S2 | 1 Bread | cash 2.50 | 2.50 |
//! | S3 | 1.250 kg Flour | cash 2.25 | 2.25 |
//! | S4 | 3 Cola | card 3.60, ref AUTH 001 | 3.60 |
//! | S5 | 2 Bread | card 3.00 + cash 5.00 (change 3.00) | 5.00 |
//! | S6 | 1 Cola, 10% line discount | cash 1.08 | 1.08 |
//! | S7 | 2 Bread + 1 Cola, 10% order discount | cash 10.00 (change 4.42) | 5.58 |
//! | S8 | 1 Gum (no stock: negative, flagged) | cash 0.75 | 0.75 |
//! | S9 | 1 Cola | card 1.20 | 1.20 |
//! | S10 | 0.500 kg Flour | cash 1.00 (change 0.10) | 0.90 |
//! | S11 | 1 Bread | bank transfer 2.50 | 2.50 |
//! | S12 | 2 Cola | cash 2.40 | 2.40 |
//!
//! Sales 30.16. Cash kept 19.86 (9 tenders), card 7.80 (3), bank transfer 2.50 (1).
//! Discounts 0.74. VAT, rounded per sale: 5.04. Returns: R1 1 Bread from S2 restocked,
//! 2.50 cash; R2 1 Cola from S4 damaged, 1.20 to the card. Expected cash
//! 100.00 + 19.86 − 2.50 = 117.36; counted 117.00, so 0.36 short.

mod common;

use pos_application::cart::CartService;
use pos_application::cash::CashService;
use pos_application::catalog::CatalogService;
use pos_application::checkout::{CheckoutService, CompleteSale, CompletedSale};
use pos_application::context::CommandContext;
use pos_application::printing::PrintService;
use pos_application::records::PrintStatus;
use pos_application::returns::{CreateReturn, ReturnService};
use pos_application::stock::StockService;
use pos_device_virtual::printer::{Scenario, VirtualPrinter};
use pos_domain::cart::AddVia;
use pos_domain::catalog::{ProductInput, ProductType, Unit, VariantInput};
use pos_domain::pricing::Discount;
use pos_domain::returns::{Disposition, ReturnLineRequest, ReturnReason};
use pos_domain::settlement::{TenderInput, TenderKind};
use pos_domain::stock_docs::{AdjustReason, DocumentDraft, DocumentLine, DocumentType};
use pos_domain::{Money, Quantity, RatePpm, VariantId};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

fn pay(kind: TenderKind, minor: i64, reference: Option<&str>) -> TenderInput {
    TenderInput {
        kind,
        amount: Money::from_minor(minor),
        external_reference: reference.map(Into::into),
    }
}

struct Day {
    shop: common::Shop,
    carts: Arc<CartService>,
    items: BTreeMap<&'static str, VariantId>,
}

impl Day {
    fn owner(&self) -> &CommandContext {
        &self.shop.owner
    }

    async fn sell(
        &self,
        lines: &[(&str, i64)],
        line_discount_pct: Option<u32>,
        order_discount_pct: Option<u32>,
        tenders: Vec<TenderInput>,
    ) -> CompletedSale {
        let ctx = self.owner();
        let mut last = None;
        for (name, milli) in lines {
            let (view, _) = self
                .carts
                .add_variant(
                    ctx,
                    self.items[name],
                    Some(Quantity::from_milli(*milli)),
                    AddVia::Search,
                )
                .await
                .unwrap();
            last = view.cart.lines.last().map(|l| l.id);
        }
        if let Some(p) = line_discount_pct {
            self.carts
                .set_line_discount(
                    ctx,
                    last.unwrap(),
                    Some(Discount::percent(RatePpm::new(p * 10_000).unwrap())),
                )
                .await
                .unwrap();
        }
        if let Some(p) = order_discount_pct {
            self.carts
                .set_order_discount(ctx, Some(Discount::percent(RatePpm::new(p * 10_000).unwrap())))
                .await
                .unwrap();
        }
        let cart = self.carts.current(ctx).await.unwrap().cart;
        let s = self.shop.store.clone();
        CheckoutService {
            carts: self.carts.clone(),
            store: Arc::new(s.clone()),
            setup: Arc::new(s.clone()),
            stock: Arc::new(s.clone()),
            receipts: Arc::new(s),
        }
        .complete_sale(
            ctx,
            CompleteSale {
                cart_id: cart.id,
                revision: cart.revision,
                tenders,
            },
        )
        .await
        .unwrap()
    }

    async fn return_one(&self, receipt: &str, item: &str, disposition: Disposition, refund: TenderInput) {
        let s = self.shop.store.clone();
        let r = ReturnService {
            store: Arc::new(s.clone()),
            returns: Arc::new(s.clone()),
            setup: Arc::new(s.clone()),
            receipts: Arc::new(s.clone()),
            pricing: Arc::new(s),
        };
        let sale = r.find(self.owner(), receipt).await.unwrap();
        let line = sale.lines.iter().find(|l| l.name == item).unwrap();
        r.create(
            self.owner(),
            CreateReturn {
                sale_id: sale.sale_id,
                lines: vec![ReturnLineRequest {
                    sale_line_id: line.sale_line_id,
                    quantity: Quantity::from_milli(1000),
                    disposition,
                }],
                reason: ReturnReason::ChangedMind,
                note: None,
                refunds: vec![refund],
            },
        )
        .await
        .unwrap();
    }

    fn stock(&self) -> StockService {
        let s = self.shop.store.clone();
        StockService {
            store: Arc::new(s.clone()),
            carts: Arc::new(s.clone()),
            stock: Arc::new(s),
        }
    }

    async fn post(&self, doc_type: DocumentType, reason: Option<AdjustReason>, lines: &[(&str, i64)]) {
        self.stock()
            .post(
                self.owner(),
                DocumentDraft {
                    doc_type,
                    reason,
                    supplier_name: None,
                    reference: None,
                    note: None,
                    lines: lines
                        .iter()
                        .map(|(n, m)| DocumentLine {
                            variant_id: self.items[n],
                            quantity: Quantity::from_milli(*m),
                            unit_cost: None,
                        })
                        .collect(),
                },
            )
            .await
            .unwrap();
    }

    async fn scalar(&self, sql: &str) -> i64 {
        sqlx::query_scalar(sql)
            .fetch_one(self.shop.store.db().readers())
            .await
            .unwrap()
    }

    async fn on_hand(&self, item: &str, bucket: &str) -> i64 {
        sqlx::query_scalar(
            "SELECT coalesce(sum(on_hand_milli), 0) FROM stock_balances WHERE variant_id = ? AND bucket = ?",
        )
        .bind(self.items[item].to_string())
        .bind(bucket)
        .fetch_one(self.shop.store.db().readers())
        .await
        .unwrap()
    }
}

#[tokio::test]
async fn the_stage_2_end_of_day_reconciles() {
    let shop = common::shop().await;
    let s = shop.store.clone();
    let catalog = CatalogService {
        store: Arc::new(s.clone()),
        catalog: Arc::new(s.clone()),
    };
    let mut items = BTreeMap::new();
    for (name, price, unit) in [
        ("Cola", 120, Unit::Ea),
        ("Bread", 250, Unit::Ea),
        ("Flour", 180, Unit::Kg),
        ("Gum", 75, Unit::Ea),
    ] {
        let p = catalog
            .create(
                &shop.owner,
                ProductInput {
                    name: name.into(),
                    description: None,
                    category_id: None,
                    product_type: ProductType::Stock,
                    unit,
                    tax_category_id: shop.tax_category,
                    quick_select: false,
                    variants: vec![VariantInput::simple(Money::from_minor(price))],
                },
            )
            .await
            .unwrap();
        items.insert(name, p.variants[0].id);
    }
    sqlx::query("UPDATE device_identity SET mode = 'live'")
        .execute(s.db().writer())
        .await
        .unwrap();
    let day = Day {
        carts: Arc::new(CartService::new(
            Arc::new(s.clone()),
            Arc::new(s.clone()),
            Arc::new(s.clone()),
        )),
        items,
        shop,
    };
    let cash = CashService {
        store: Arc::new(s.clone()),
        cash: Arc::new(s.clone()),
        receipts: Arc::new(s.clone()),
        setup: Arc::new(s.clone()),
        pricing: Arc::new(s.clone()),
    };
    let outbox_before = day.scalar("SELECT count(*) FROM outbox_events").await;

    // Opening stock (no gum: selling it goes negative).
    day.post(
        DocumentType::Opening,
        None,
        &[("Cola", 50_000), ("Bread", 10_000), ("Flour", 20_000)],
    )
    .await;

    // 1. Open the drawer with 100.00.
    cash.open(day.owner(), Money::from_minor(10_000)).await.unwrap();

    // 2. Twelve sales.
    use TenderKind::*;
    let mut sales = Vec::new();
    sales.push(
        day.sell(&[("Cola", 2000)], None, None, vec![pay(Cash, 500, None)])
            .await,
    );
    sales.push(
        day.sell(&[("Bread", 1000)], None, None, vec![pay(Cash, 250, None)])
            .await,
    );
    sales.push(
        day.sell(&[("Flour", 1250)], None, None, vec![pay(Cash, 225, None)])
            .await,
    );
    sales.push(
        day.sell(
            &[("Cola", 3000)],
            None,
            None,
            vec![pay(CardExternal, 360, Some("AUTH 001"))],
        )
        .await,
    );
    sales.push(
        day.sell(
            &[("Bread", 2000)],
            None,
            None,
            vec![pay(CardExternal, 300, Some("AUTH 002")), pay(Cash, 500, None)],
        )
        .await,
    );
    sales.push(
        day.sell(&[("Cola", 1000)], Some(10), None, vec![pay(Cash, 108, None)])
            .await,
    );
    sales.push(
        day.sell(
            &[("Bread", 2000), ("Cola", 1000)],
            None,
            Some(10),
            vec![pay(Cash, 1000, None)],
        )
        .await,
    );
    sales.push(
        day.sell(&[("Gum", 1000)], None, None, vec![pay(Cash, 75, None)])
            .await,
    );
    sales.push(
        day.sell(&[("Cola", 1000)], None, None, vec![pay(CardExternal, 120, None)])
            .await,
    );
    sales.push(
        day.sell(&[("Flour", 500)], None, None, vec![pay(Cash, 100, None)])
            .await,
    );
    sales.push(
        day.sell(
            &[("Bread", 1000)],
            None,
            None,
            vec![pay(BankTransferExternal, 250, Some("TRF 77"))],
        )
        .await,
    );
    sales.push(
        day.sell(&[("Cola", 2000)], None, None, vec![pay(Cash, 240, None)])
            .await,
    );
    let totals: Vec<i64> = sales.iter().map(|s| s.sale.total.minor()).collect();
    assert_eq!(totals, [240, 250, 225, 360, 500, 108, 558, 75, 120, 90, 250, 240]);
    let changes: Vec<i64> = sales.iter().map(|s| s.sale.change.minor()).collect();
    assert_eq!(changes, [260, 0, 0, 0, 300, 0, 442, 0, 0, 10, 0, 0]);
    assert_eq!(
        sales[7].negative_stock.len(),
        1,
        "gum went below zero and was flagged"
    );
    let numbers: Vec<&str> = sales.iter().map(|s| s.receipt_number.as_str()).collect();
    assert_eq!(numbers.first(), Some(&"R01-000001"));
    assert_eq!(numbers.last(), Some(&"R01-000012"));

    // 3. Receive stock. 4. One adjustment.
    day.post(DocumentType::Receipt, None, &[("Cola", 24_000), ("Gum", 12_000)])
        .await;
    day.post(
        DocumentType::Adjustment,
        Some(AdjustReason::Damage),
        &[("Bread", 1000)],
    )
    .await;

    // 5. Two returns: restocked with a cash refund; damaged, refunded to the card.
    day.return_one(
        &sales[1].receipt_number,
        "Bread",
        Disposition::Restock,
        pay(Cash, 250, None),
    )
    .await;
    day.return_one(
        &sales[3].receipt_number,
        "Cola",
        Disposition::Damaged,
        pay(CardExternal, 120, Some("REFUND 9")),
    )
    .await;

    // 6. A print with no answer; the operator says it did not print; the reprint prints.
    let captures = tempfile::tempdir().unwrap();
    let printer = Arc::new(VirtualPrinter::new(captures.path()));
    let prints = PrintService {
        store: Arc::new(s.clone()),
        prints: Arc::new(s.clone()),
        receipts: Arc::new(s.clone()),
        printer: printer.clone(),
        timeout: Duration::from_millis(100),
    };
    printer.script([Scenario::Timeout]);
    let job = prints
        .print_sale(day.owner(), Some(sales[11].sale.id))
        .await
        .unwrap();
    assert_eq!(job.status, PrintStatus::Unknown);
    let reprint = prints
        .resolve_unknown(day.owner(), job.job_id, false)
        .await
        .unwrap();
    assert_eq!((reprint.status, reprint.reprint), (PrintStatus::Completed, true));
    assert_eq!(printer.submissions(), 2);
    let reprint_text = std::fs::read_to_string(printer.out_dir().join("R01-000012-reprint.txt")).unwrap();
    assert!(reprint_text.contains("REPRINT"));

    // Before closing: what the manager sees.
    let state = cash.current(day.owner()).await.unwrap();
    assert_eq!(state.expected.unwrap().minor(), 11_736);

    // 7. Close with 117.00 counted.
    let closed = cash
        .close(day.owner(), Money::from_minor(11_700), None)
        .await
        .unwrap();
    assert_eq!(
        (closed.expected.unwrap().minor(), closed.variance.unwrap().minor()),
        (11_736, -36)
    );

    // The tender mix and the day's figures (Z summary).
    let t = closed.totals.unwrap();
    assert_eq!((t.sales_count, t.sales_total.minor()), (12, 3016));
    assert_eq!(t.discount_total.minor(), 12 + 62);
    assert_eq!(t.tax_total.minor(), 504);
    let mix: Vec<(&str, u32, i64)> = t
        .tenders
        .iter()
        .map(|x| (x.kind.as_str(), x.count, x.amount.minor()))
        .collect();
    assert_eq!(
        mix,
        [
            ("bank_transfer_external", 1, 250),
            ("card_external", 3, 780),
            ("cash", 9, 1986)
        ]
    );
    assert_eq!((t.returns_count, t.refunds_total.minor()), (2, 370));
    let refunds: Vec<(&str, u32, i64)> = t
        .refunds
        .iter()
        .map(|x| (x.kind.as_str(), x.count, x.amount.minor()))
        .collect();
    assert_eq!(refunds, [("card_external", 1, 120), ("cash", 1, 250)]);
    let z = cash.z_summary(day.owner(), None, Some(80)).await.unwrap();
    let z_text: String = z.lines.iter().map(|l| format!("{}\n", l.text)).collect();
    for expected in [
        "Sales (12)",
        "30.16",
        "EXPECTED CASH",
        "117.36",
        "COUNTED CASH",
        "117.00",
        "VARIANCE (SHORT)",
        "-0.36",
    ] {
        assert!(z_text.contains(expected), "{expected} missing from\n{z_text}");
    }

    // E14: the daily report for the day (docs/reporting-definitions.md), to the minor unit.
    let reports = pos_application::reports::ReportService {
        reports: Arc::new(s.clone()),
    };
    let today: pos_domain::time::BusinessDate = "2026-01-01".parse().unwrap();
    let summary = reports.sales_summary(day.owner(), today, today).await.unwrap();
    assert_eq!(summary.days.len(), 1);
    let f = &summary.total;
    assert_eq!((f.sales_count, f.returns_count), (12, 2));
    assert_eq!(f.gross.minor(), 3090); // sales total + discounts (prices include tax)
    assert_eq!(
        (
            f.line_discounts.minor(),
            f.order_discounts.minor(),
            f.discounts().minor()
        ),
        (12, 62, 74)
    );
    assert_eq!(
        (f.tax.minor(), f.rounding.minor(), f.sales_total.minor()),
        (504, 0, 3016)
    );
    assert_eq!((f.refunds.minor(), f.refunded_tax.minor()), (370, 42 + 20));
    assert_eq!(f.net_incl_tax().minor(), 3016 - 370); // 26.46
    assert_eq!(f.net_excl_tax().minor(), 2646 - (504 - 62)); // 22.04
    let mix: Vec<(&str, &str, u32, i64, u32, i64, i64)> = reports
        .tender_mix(day.owner(), today, today)
        .await
        .unwrap()
        .iter()
        .map(|r| {
            (
                r.kind.as_str(),
                if r.confirmation_source == "operator" {
                    "operator"
                } else {
                    "provider"
                },
                r.count,
                r.taken.minor(),
                r.refund_count,
                r.refunded.minor(),
                r.net().minor(),
            )
        })
        .collect();
    assert_eq!(
        mix,
        [
            ("bank_transfer_external", "operator", 1, 250, 0, 0, 250),
            ("card_external", "operator", 3, 780, 1, 120, 660),
            ("cash", "operator", 9, 1986, 1, 250, 1736),
        ]
    );
    let top = |by| {
        let r = &reports;
        let o = day.owner();
        async move {
            r.top_products(o, today, today, by, 10)
                .await
                .unwrap()
                .iter()
                .map(|p| (p.name.clone(), p.quantity.milli(), p.value.minor()))
                .collect::<Vec<_>>()
        }
    };
    use pos_application::reports::TopBy;
    // Returns are netted: 10 Cola sold, 1 back; 6 Bread sold, 1 back.
    assert_eq!(
        top(TopBy::Quantity).await,
        [
            ("Cola".to_owned(), 9_000, 1176 - 120),
            ("Bread".to_owned(), 5_000, 1450 - 250),
            ("Flour".to_owned(), 1_750, 315),
            ("Gum".to_owned(), 1_000, 75),
        ]
    );
    let by_value: Vec<String> = top(TopBy::Value).await.into_iter().map(|r| r.0).collect();
    assert_eq!(by_value, ["Bread", "Cola", "Flour", "Gum"]);

    // Stock balances: opening − sold + received ± adjustment ± returns.
    assert_eq!(day.on_hand("Cola", "sellable").await, 50_000 - 10_000 + 24_000);
    assert_eq!(day.on_hand("Cola", "damaged").await, 1_000);
    assert_eq!(
        day.on_hand("Bread", "sellable").await,
        10_000 - 6_000 - 1_000 + 1_000
    );
    assert_eq!(day.on_hand("Bread", "damaged").await, 1_000);
    assert_eq!(day.on_hand("Flour", "sellable").await, 20_000 - 1_750);
    assert_eq!(day.on_hand("Gum", "sellable").await, -1_000 + 12_000);
    // Projection verify (E11-T06): no drift.
    assert!(day.stock().verify(day.owner()).await.unwrap().is_empty());

    // The outbox: exactly the day's events, numbered without gaps.
    let by_type: Vec<(String, i64)> = sqlx::query_as(
        "SELECT event_type, count(*) FROM outbox_events WHERE event_type IN
           ('sale.completed', 'return.completed', 'inventory.document_posted', 'cash_session.opened', 'cash_session.closed')
         GROUP BY event_type ORDER BY event_type",
    )
    .fetch_all(s.db().readers())
    .await
    .unwrap();
    assert_eq!(
        by_type,
        [
            ("cash_session.closed".to_owned(), 1),
            ("cash_session.opened".to_owned(), 1),
            ("inventory.document_posted".to_owned(), 3),
            ("return.completed".to_owned(), 2),
            ("sale.completed".to_owned(), 12),
        ]
    );
    assert_eq!(
        day.scalar("SELECT count(*) FROM outbox_events").await - outbox_before,
        19
    );
    let (count, min, max): (i64, i64, i64) =
        sqlx::query_as("SELECT count(*), min(local_sequence), max(local_sequence) FROM outbox_events")
            .fetch_one(s.db().readers())
            .await
            .unwrap();
    assert_eq!((min, max), (1, count), "outbox sequence is gapless");

    // E17-T01: every event this day wrote matches the sync contract.
    let seen = common::contract::check_outbox(&day.shop.store).await;
    for t in [
        "sale.completed",
        "return.completed",
        "inventory.document_posted",
        "cash_session.opened",
        "cash_session.closed",
    ] {
        assert!(seen.contains(t), "{t} not exercised");
    }
}
