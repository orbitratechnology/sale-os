//! E11-T06 (INV-004): stock balances are a projection of the movements. `verify` finds any
//! balance that differs from the sum of its movements; `rebuild` recomputes them all.

mod common;

use pos_application::cart::CartService;
use pos_application::catalog::CatalogService;
use pos_application::checkout::{CheckoutService, CompleteSale};
use pos_application::returns::{CreateReturn, ReturnService};
use pos_application::scan::ScanSource;
use pos_application::stock::StockService;
use pos_domain::catalog::{ProductInput, ProductType, Unit, VariantInput};
use pos_domain::returns::{Disposition, ReturnLineRequest, ReturnReason};
use pos_domain::settlement::{TenderInput, TenderKind};
use pos_domain::stock_docs::{AdjustReason, DocumentDraft, DocumentLine, DocumentType};
use pos_domain::{ErrorCode, HasErrorCode, Money, Quantity, Role, ScanEventId, UuidV7Generator, VariantId};
use std::sync::Arc;

struct Till {
    shop: common::Shop,
    carts: Arc<CartService>,
    stock: StockService,
    items: Vec<(String, VariantId)>,
}

async fn till() -> Till {
    let shop = common::shop().await;
    let s = shop.store.clone();
    let catalog = CatalogService {
        store: Arc::new(s.clone()),
        catalog: Arc::new(s.clone()),
    };
    let mut items = Vec::new();
    for (i, name) in ["Tea", "Cake", "Jam", "Bread"].iter().enumerate() {
        let mut v = VariantInput::simple(Money::from_minor(100 + 37 * i as i64));
        v.sku = Some(name.to_uppercase());
        let p = catalog
            .create(
                &shop.owner,
                ProductInput {
                    name: (*name).into(),
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
        items.push((name.to_uppercase(), p.variants[0].id));
    }
    Till {
        carts: Arc::new(CartService::new(
            Arc::new(s.clone()),
            Arc::new(s.clone()),
            Arc::new(s.clone()),
        )),
        stock: StockService {
            store: Arc::new(s.clone()),
            carts: Arc::new(s.clone()),
            stock: Arc::new(s),
        },
        items,
        shop,
    }
}

fn doc(doc_type: DocumentType, reason: Option<AdjustReason>, v: VariantId, milli: i64) -> DocumentDraft {
    DocumentDraft {
        doc_type,
        reason,
        supplier_name: None,
        reference: None,
        note: None,
        lines: vec![DocumentLine {
            variant_id: v,
            quantity: Quantity::from_milli(milli),
            unit_cost: None,
        }],
    }
}

/// A tiny deterministic generator (no seed files, same run every time).
struct Lcg(u64);
impl Lcg {
    fn next(&mut self, n: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) % n
    }
}

impl Till {
    async fn sell(&self, sku: &str, units: usize) -> String {
        let owner = &self.shop.owner;
        for _ in 0..units {
            self.carts
                .scan(
                    owner,
                    sku,
                    ScanEventId::generate(&UuidV7Generator),
                    ScanSource::Manual,
                )
                .await
                .unwrap();
        }
        let cart = self.carts.current(owner).await.unwrap().cart;
        let s = self.shop.store.clone();
        CheckoutService {
            carts: self.carts.clone(),
            store: Arc::new(s.clone()),
            setup: Arc::new(s.clone()),
            stock: Arc::new(s.clone()),
            receipts: Arc::new(s),
        }
        .complete_sale(
            owner,
            CompleteSale {
                cart_id: cart.id,
                revision: cart.revision,
                tenders: vec![TenderInput {
                    kind: TenderKind::Cash,
                    amount: Money::from_minor(100_000),
                    external_reference: None,
                }],
            },
        )
        .await
        .unwrap()
        .receipt_number
    }

    async fn return_one(&self, receipt: &str, damaged: bool) -> bool {
        let s = self.shop.store.clone();
        let r = ReturnService {
            store: Arc::new(s.clone()),
            returns: Arc::new(s.clone()),
            setup: Arc::new(s.clone()),
            receipts: Arc::new(s.clone()),
            pricing: Arc::new(s),
        };
        let sale = r.find(&self.shop.owner, receipt).await.unwrap();
        let Some(line) = sale.lines.iter().find(|l| l.available().milli() >= 1000) else {
            return false;
        };
        let plan_line = ReturnLineRequest {
            sale_line_id: line.sale_line_id,
            quantity: Quantity::from_milli(1000),
            disposition: if damaged {
                Disposition::Damaged
            } else {
                Disposition::Restock
            },
        };
        // The cumulative rule may refund a minor unit more or less: ask for the preview.
        let plan = r
            .preview(&self.shop.owner, sale.sale_id, std::slice::from_ref(&plan_line))
            .await
            .unwrap();
        r.create(
            &self.shop.owner,
            CreateReturn {
                sale_id: sale.sale_id,
                lines: vec![plan_line],
                reason: ReturnReason::ChangedMind,
                note: None,
                refunds: vec![TenderInput {
                    kind: TenderKind::Cash,
                    amount: plan.total,
                    external_reference: None,
                }],
            },
        )
        .await
        .unwrap();
        true
    }

    async fn sum_movements(&self, v: VariantId, bucket: &str) -> i64 {
        sqlx::query_scalar("SELECT coalesce(sum(quantity_delta_milli), 0) FROM stock_movements WHERE variant_id = ? AND bucket = ?")
            .bind(v.to_string())
            .bind(bucket)
            .fetch_one(self.shop.store.db().readers())
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn a_random_mix_of_everything_never_drifts() {
    for seed in [7_u64, 1_234, 99_991] {
        let t = till().await;
        let owner = &t.shop.owner;
        let mut rng = Lcg(seed);
        let mut receipts: Vec<String> = Vec::new();
        for _ in 0..60 {
            let i = rng.next(t.items.len() as u64) as usize;
            let (sku, v) = (&t.items[i].0, t.items[i].1);
            let units = 1 + rng.next(3) as i64;
            match rng.next(7) {
                0 | 1 => receipts.push(t.sell(sku, units as usize).await),
                2 if !receipts.is_empty() => {
                    let r = receipts[rng.next(receipts.len() as u64) as usize].clone();
                    t.return_one(&r, rng.next(2) == 0).await;
                }
                3 => {
                    t.stock
                        .post(owner, doc(DocumentType::Receipt, None, v, units * 1000))
                        .await
                        .unwrap();
                }
                4 => {
                    let reason = [
                        AdjustReason::Damage,
                        AdjustReason::Loss,
                        AdjustReason::CountCorrection,
                    ][rng.next(3) as usize];
                    let sign = if reason == AdjustReason::CountCorrection && rng.next(2) == 0 {
                        -1
                    } else {
                        1
                    };
                    t.stock
                        .post(
                            owner,
                            doc(DocumentType::Adjustment, Some(reason), v, sign * units * 1000),
                        )
                        .await
                        .unwrap();
                }
                5 => {
                    // Write off only what is in damaged stock.
                    if t.sum_movements(v, "damaged").await >= 1000 {
                        t.stock
                            .post(owner, doc(DocumentType::WriteOff, None, v, 1000))
                            .await
                            .unwrap();
                    }
                }
                _ => {
                    t.stock
                        .post(owner, doc(DocumentType::Opening, None, v, units * 1000))
                        .await
                        .unwrap();
                }
            }
        }
        // INV-004: every balance equals the sum of its movements.
        assert_eq!(t.stock.verify(owner).await.unwrap(), [], "seed {seed}");
        let movements: i64 = sqlx::query_scalar("SELECT count(*) FROM stock_movements")
            .fetch_one(t.shop.store.db().readers())
            .await
            .unwrap();
        assert!(movements > 40, "seed {seed}: only {movements} movements");
    }
}

#[tokio::test]
async fn verify_finds_a_tampered_balance_and_rebuild_repairs_it() {
    let t = till().await;
    let owner = &t.shop.owner;
    let (tea, cake) = (t.items[0].1, t.items[1].1);
    t.stock
        .post(owner, doc(DocumentType::Receipt, None, tea, 10_000))
        .await
        .unwrap();
    t.stock
        .post(
            owner,
            doc(DocumentType::Adjustment, Some(AdjustReason::Damage), tea, 2_000),
        )
        .await
        .unwrap();
    t.sell("CAKE", 1).await;
    assert!(t.stock.verify(owner).await.unwrap().is_empty());

    // Something outside the write path changes the projection: one row wrong, one missing.
    let w = t.shop.store.db().writer();
    sqlx::query("UPDATE stock_balances SET on_hand_milli = 999 WHERE variant_id = ? AND bucket = 'sellable'")
        .bind(tea.to_string())
        .execute(w)
        .await
        .unwrap();
    sqlx::query("DELETE FROM stock_balances WHERE variant_id = ?")
        .bind(cake.to_string())
        .execute(w)
        .await
        .unwrap();
    let mut drift = t.stock.verify(&t.shop.as_role(Role::Cashier)).await.unwrap();
    drift.sort_by_key(|d| d.recorded.milli());
    assert_eq!(drift.len(), 2);
    assert_eq!(
        (
            drift[0].variant_id,
            drift[0].recorded.milli(),
            drift[0].from_movements.milli()
        ),
        (cake, 0, -1_000)
    );
    assert_eq!(
        (
            drift[1].variant_id,
            drift[1].recorded.milli(),
            drift[1].from_movements.milli()
        ),
        (tea, 999, 8_000)
    );

    // Only the owner rebuilds; it reports what was wrong, audits it, and leaves no drift.
    for role in [Role::Cashier, Role::Manager] {
        let e = t.stock.rebuild(&t.shop.as_role(role)).await.unwrap_err();
        assert_eq!(e.error_code(), ErrorCode::PermissionDenied);
    }
    let fixed = t.stock.rebuild(owner).await.unwrap();
    assert_eq!(fixed.len(), 2);
    assert!(t.stock.verify(owner).await.unwrap().is_empty());
    assert_eq!(t.sum_movements(tea, "sellable").await, 8_000);
    let audited: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE action = 'inventory.projection_rebuilt' AND json_extract(metadata_json, '$.driftBefore') = 2",
    )
    .fetch_one(t.shop.store.db().readers())
    .await
    .unwrap();
    assert_eq!(audited, 1);
}

#[tokio::test]
async fn the_startup_check_is_bounded_and_reports_drift() {
    use pos_local_db::StartupStockCheck;
    use std::time::Duration;
    let t = till().await;
    let store = &t.shop.store;
    t.stock
        .post(
            &t.shop.owner,
            doc(DocumentType::Receipt, None, t.items[0].1, 5_000),
        )
        .await
        .unwrap();
    assert_eq!(
        store.startup_stock_check(Duration::from_secs(2)).await.unwrap(),
        StartupStockCheck::Healthy
    );
    sqlx::query("UPDATE stock_balances SET on_hand_milli = 1")
        .execute(store.db().writer())
        .await
        .unwrap();
    assert_eq!(
        store.startup_stock_check(Duration::from_secs(2)).await.unwrap(),
        StartupStockCheck::Drift(1)
    );
    // No time at all: deferred, never blocking start-up.
    assert_eq!(
        store.startup_stock_check(Duration::ZERO).await.unwrap(),
        StartupStockCheck::Deferred
    );
}

#[tokio::test]
async fn the_history_names_every_source_newest_first() {
    let t = till().await;
    let owner = &t.shop.owner;
    let tea = t.items[0].1;
    t.stock
        .post(owner, doc(DocumentType::Receipt, None, tea, 10_000))
        .await
        .unwrap();
    t.shop.clock.advance_ms(60_000);
    let receipt = t.sell("TEA", 2).await;
    t.shop.clock.advance_ms(60_000);
    assert!(t.return_one(&receipt, true).await);
    t.shop.clock.advance_ms(60_000);
    t.stock
        .post(
            owner,
            doc(DocumentType::Adjustment, Some(AdjustReason::Damage), tea, 1_000),
        )
        .await
        .unwrap();

    let rows: Vec<(String, String, i64, String, String)> = t
        .stock
        .history(owner, tea, 50)
        .await
        .unwrap()
        .into_iter()
        .map(|r| {
            (
                r.movement_type,
                r.bucket,
                r.delta.milli(),
                r.source_number,
                r.actor_name,
            )
        })
        .collect();
    let olu = "Olu".to_owned();
    assert_eq!(
        rows,
        [
            (
                "damage".into(),
                "damaged".into(),
                1_000,
                "R01-IN-000002".into(),
                olu.clone()
            ),
            (
                "damage".into(),
                "sellable".into(),
                -1_000,
                "R01-IN-000002".into(),
                olu.clone()
            ),
            (
                "return_damaged".into(),
                "damaged".into(),
                1_000,
                "R01-R-000001".into(),
                olu.clone()
            ),
            (
                "sale".into(),
                "sellable".into(),
                -2_000,
                receipt.clone(),
                olu.clone()
            ),
            (
                "receipt".into(),
                "sellable".into(),
                10_000,
                "R01-IN-000001".into(),
                olu
            ),
        ]
    );
    // Cashiers can see inventory; the limit applies.
    let cashier = t.shop.as_role(Role::Cashier);
    assert_eq!(t.stock.history(&cashier, tea, 2).await.unwrap().len(), 2);
}
