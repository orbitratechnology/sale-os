//! E20-T06: items sold by weight take their quantity from the scale. Only a stable, positive
//! weight is used; the line gets the weight exactly and is priced like any line (ADR-004).

mod common;

use pos_application::cart::{CartService, CartView};
use pos_application::catalog::CatalogService;
use pos_application::scale::{Scale, WeighRefusal, Weighed};
use pos_application::scan::{ScanOutcome, ScanSource};
use pos_device_ports::{DeviceError, DeviceHealth, ScalePort, ScaleReading, Weight};
use pos_domain::cart::AddVia;
use pos_domain::catalog::{BarcodeInput, ProductInput, ProductType, Unit, VariantInput};
use pos_domain::{Money, Quantity, ScanEventId, UuidV7Generator, VariantId};
use std::sync::{Arc, Mutex};

/// A scale that answers from a script; the last answer repeats.
struct ScriptedScale {
    script: Mutex<Vec<Result<ScaleReading, DeviceError>>>,
    reads: Mutex<usize>,
}

impl ScriptedScale {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            script: Mutex::new(vec![Ok(ScaleReading::Stable(Weight::ZERO))]),
            reads: Mutex::new(0),
        })
    }

    fn answer(&self, script: Vec<Result<ScaleReading, DeviceError>>) {
        *self.script.lock().unwrap() = script;
    }

    fn reads(&self) -> usize {
        *self.reads.lock().unwrap()
    }
}

#[async_trait::async_trait]
impl ScalePort for ScriptedScale {
    fn describe(&self) -> String {
        "COM3 @ 9600, Toledo 8217 (kg)".into()
    }
    async fn read(&self) -> Result<ScaleReading, DeviceError> {
        *self.reads.lock().unwrap() += 1;
        let mut q = self.script.lock().unwrap();
        if q.len() > 1 { q.remove(0) } else { q[0].clone() }
    }
    fn health(&self) -> tokio::sync::watch::Receiver<DeviceHealth> {
        tokio::sync::watch::channel(DeviceHealth::Ready).1
    }
}

struct Till {
    shop: common::Shop,
    carts: CartService,
    scale: Arc<Scale>,
    device: Arc<ScriptedScale>,
    /// Bananas at 2.99 a kilogram (`kg`), SKU BANANA.
    bananas: VariantId,
    /// Cheese at 0.02 a gram (`g`).
    cheese: VariantId,
    /// Cola, sold by the piece (`ea`), barcode 4006381333931.
    cola: VariantId,
}

async fn product(
    catalog: &CatalogService,
    shop: &common::Shop,
    name: &str,
    unit: Unit,
    price: i64,
    code: &str,
) -> VariantId {
    let mut v = VariantInput::simple(Money::from_minor(price));
    v.sku = Some(code.to_uppercase());
    v.barcodes = vec![BarcodeInput::alias(code)];
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
                variants: vec![v],
            },
        )
        .await
        .unwrap();
    p.variants[0].id
}

async fn till() -> Till {
    let shop = common::shop().await;
    let s = shop.store.clone();
    let catalog = CatalogService {
        store: Arc::new(s.clone()),
        catalog: Arc::new(s.clone()),
    };
    let bananas = product(&catalog, &shop, "Bananas", Unit::Kg, 299, "banana").await;
    let cheese = product(&catalog, &shop, "Cheddar", Unit::G, 2, "cheddar").await;
    let cola = product(&catalog, &shop, "Cola 330ml", Unit::Ea, 120, "4006381333931").await;
    let scale = Arc::new(Scale::default());
    let device = ScriptedScale::new();
    scale.set(Some(device.clone()));
    let carts =
        CartService::new(Arc::new(s.clone()), Arc::new(s.clone()), Arc::new(s)).with_scale(scale.clone());
    Till {
        shop,
        carts,
        scale,
        device,
        bananas,
        cheese,
        cola,
    }
}

fn mg(n: i64) -> Weight {
    Weight::from_milligrams(n)
}

fn done(w: Weighed) -> (CartView, pos_domain::CartLineId, Weight) {
    match w {
        Weighed::Done { view, line, weight } => (*view, line, weight),
        Weighed::Refused(r) => panic!("refused: {r:?}"),
    }
}

fn line_of(view: &CartView, line: pos_domain::CartLineId) -> (Quantity, Money, Money) {
    let l = view.cart.lines.iter().find(|l| l.id == line).unwrap();
    let p = view.priced.lines.iter().find(|p| p.line_id == line).unwrap();
    (l.quantity, p.gross, p.tax)
}

#[tokio::test]
async fn a_weighed_item_gets_the_exact_weight_and_its_price() {
    let t = till().await;
    // 1.234 kg of bananas at 2.99/kg: 3.68966, rounded half away from zero to 3.69 (ADR-004);
    // VAT inside at 20%: 0.615 → 0.62.
    t.device.answer(vec![Ok(ScaleReading::Stable(mg(1_234_000)))]);
    let (view, line, weight) = done(
        t.carts
            .add_weighed(&t.shop.owner, t.bananas, AddVia::Search)
            .await
            .unwrap(),
    );
    assert_eq!(weight, mg(1_234_000));
    assert_eq!(
        line_of(&view, line),
        (
            Quantity::from_milli(1_234),
            Money::from_minor(369),
            Money::from_minor(62)
        )
    );
    assert_eq!(view.priced.total, Money::from_minor(369));

    // Weighed again: a line of its own, not merged into the first.
    t.device.answer(vec![Ok(ScaleReading::Stable(mg(500_000)))]);
    let (view, second, _) = done(
        t.carts
            .add_weighed(&t.shop.owner, t.bananas, AddVia::QuickSelect)
            .await
            .unwrap(),
    );
    assert_ne!(second, line);
    assert_eq!(view.cart.lines.len(), 2);
    // 0.5 kg × 2.99 = 1.495 → 1.50.
    assert_eq!(line_of(&view, second).0, Quantity::from_milli(500));
    assert_eq!(line_of(&view, second).1, Money::from_minor(150));
    assert_eq!(view.priced.total, Money::from_minor(519));

    // A gram item: 250.5 g of cheese at 0.02/g is 5.01; its quantity is in milligrams.
    t.device.answer(vec![Ok(ScaleReading::Stable(mg(250_500)))]);
    let (view, cheese, _) = done(
        t.carts
            .add_weighed(&t.shop.owner, t.cheese, AddVia::Search)
            .await
            .unwrap(),
    );
    assert_eq!(line_of(&view, cheese).0, Quantity::from_milli(250_500));
    assert_eq!(line_of(&view, cheese).1, Money::from_minor(501));

    // A kg item weighed finer than a gram: 0.3335 kg is 334 g (half away from zero), 0.99866 → 1.00.
    t.device.answer(vec![Ok(ScaleReading::Stable(mg(333_500)))]);
    let (view, fine, _) = done(
        t.carts
            .add_weighed(&t.shop.owner, t.bananas, AddVia::Search)
            .await
            .unwrap(),
    );
    assert_eq!(line_of(&view, fine).0, Quantity::from_milli(334));
    assert_eq!(line_of(&view, fine).1, Money::from_minor(100));

    // The cart as stored reads back the same.
    let again = t.carts.current(&t.shop.owner).await.unwrap();
    assert_eq!(again.priced.total, view.priced.total);
    assert_eq!(again.cart.lines.len(), 4);
}

#[tokio::test]
async fn only_a_stable_positive_weight_is_sold() {
    let t = till().await;
    let refused = |script: Vec<Result<ScaleReading, DeviceError>>| {
        t.device.answer(script);
        let t = &t;
        async move {
            match t
                .carts
                .add_weighed(&t.shop.owner, t.bananas, AddVia::Search)
                .await
                .unwrap()
            {
                Weighed::Refused(r) => r,
                Weighed::Done { .. } => panic!("weighed"),
            }
        }
    };
    use ScaleReading::*;
    assert_eq!(refused(vec![Ok(Stable(Weight::ZERO))]).await, WeighRefusal::Zero);
    assert_eq!(
        refused(vec![Ok(Stable(mg(-15_000)))]).await,
        WeighRefusal::Negative
    );
    assert_eq!(refused(vec![Ok(Overload)]).await, WeighRefusal::Overload);
    assert_eq!(refused(vec![Ok(Underload)]).await, WeighRefusal::Underload);
    assert_eq!(
        refused(vec![Err(DeviceError::Disconnected("COM3 not found".into()))]).await,
        WeighRefusal::Failed("the device is not connected: COM3 not found".into())
    );
    // Still moving after the settle time (3 s): refused, after asking several times.
    let before = t.device.reads();
    let started = std::time::Instant::now();
    assert_eq!(
        refused(vec![Ok(Moving(Some(mg(1_000_000))))]).await,
        WeighRefusal::Unstable
    );
    assert!(started.elapsed() >= std::time::Duration::from_secs(2));
    assert!(t.device.reads() - before > 5, "asked again while it settled");
    // Less than half a gram on a kg item is no quantity at all.
    assert_eq!(refused(vec![Ok(Stable(mg(400)))]).await, WeighRefusal::Zero);
    // Nothing was added by any of these.
    assert!(
        t.carts
            .current(&t.shop.owner)
            .await
            .unwrap()
            .cart
            .lines
            .is_empty()
    );

    // Settling after a moment is fine: the stable weight is the one sold.
    t.device.answer(vec![
        Ok(Moving(Some(mg(1_100_000)))),
        Ok(Moving(None)),
        Ok(Stable(mg(1_050_000))),
    ]);
    let (view, line, _) = done(
        t.carts
            .add_weighed(&t.shop.owner, t.bananas, AddVia::Search)
            .await
            .unwrap(),
    );
    assert_eq!(line_of(&view, line).0, Quantity::from_milli(1_050));

    // No scale: refused as such; an item not sold by weight cannot be weighed.
    t.scale.set(None);
    assert_eq!(
        match t
            .carts
            .add_weighed(&t.shop.owner, t.bananas, AddVia::Search)
            .await
            .unwrap()
        {
            Weighed::Refused(r) => r,
            Weighed::Done { .. } => panic!(),
        },
        WeighRefusal::NoScale
    );
    let err = t
        .carts
        .add_weighed(&t.shop.owner, t.cola, AddVia::Search)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not by weight"), "{err}");
}

#[tokio::test]
async fn a_scanned_item_sold_by_weight_is_weighed_and_counted_once() {
    let t = till().await;
    // Without a scale, an item sold by weight scans as one unit (the cashier types the weight).
    let device = t.scale.port();
    t.scale.set(None);
    let plain = t
        .carts
        .scan(
            &t.shop.owner,
            "BANANA",
            ScanEventId::generate(&UuidV7Generator),
            ScanSource::Scanner,
        )
        .await
        .unwrap();
    let ScanOutcome::Added { view, .. } = plain else {
        panic!("{plain:?}");
    };
    assert_eq!(view.cart.lines[0].quantity, Quantity::from_milli(1_000));
    t.carts
        .remove_line(&t.shop.owner, view.cart.lines[0].id)
        .await
        .unwrap();
    t.scale.set(device);

    let id = ScanEventId::generate(&UuidV7Generator);
    let out = t
        .carts
        .scan(&t.shop.owner, "BANANA", id, ScanSource::Scanner)
        .await
        .unwrap();
    let ScanOutcome::Weigh { variant, name, unit } = out else {
        panic!("{out:?}");
    };
    assert_eq!((variant, name.as_str(), unit), (t.bananas, "Bananas", Unit::Kg));
    assert!(
        t.carts
            .current(&t.shop.owner)
            .await
            .unwrap()
            .cart
            .lines
            .is_empty()
    );
    // The same delivery again changes nothing.
    let again = t
        .carts
        .scan(&t.shop.owner, "BANANA", id, ScanSource::Scanner)
        .await
        .unwrap();
    assert!(matches!(again, ScanOutcome::Duplicate { .. }), "{again:?}");
    // The till weighs it under that scan.
    t.device.answer(vec![Ok(ScaleReading::Stable(mg(2_000_000)))]);
    let (view, line, _) = done(
        t.carts
            .add_weighed(&t.shop.owner, variant, AddVia::Scan { event_id: id })
            .await
            .unwrap(),
    );
    assert_eq!(
        line_of(&view, line),
        (
            Quantity::from_milli(2_000),
            Money::from_minor(598),
            Money::from_minor(100)
        )
    );

    // Items sold by the piece scan as before.
    let cola = t
        .carts
        .scan(
            &t.shop.owner,
            "4006381333931",
            ScanEventId::generate(&UuidV7Generator),
            ScanSource::Scanner,
        )
        .await
        .unwrap();
    assert!(matches!(cola, ScanOutcome::Added { .. }), "{cola:?}");
}

#[tokio::test]
async fn the_weigh_key_sets_the_selected_lines_quantity_and_typing_still_works() {
    let t = till().await;
    let (view, _) = t
        .carts
        .add_variant(
            &t.shop.owner,
            t.bananas,
            Some(Quantity::from_milli(1_000)),
            AddVia::Search,
        )
        .await
        .unwrap();
    let line = view.cart.lines[0].id;
    t.device.answer(vec![Ok(ScaleReading::Stable(mg(750_000)))]);
    let (view, same, _) = done(t.carts.weigh_line(&t.shop.owner, line).await.unwrap());
    assert_eq!(same, line);
    // 0.75 kg × 2.99 = 2.2425 → 2.24.
    assert_eq!(line_of(&view, line).0, Quantity::from_milli(750));
    assert_eq!(line_of(&view, line).1, Money::from_minor(224));
    // A refused weighing leaves the line as it was.
    t.device.answer(vec![Ok(ScaleReading::Overload)]);
    assert_eq!(
        t.carts.weigh_line(&t.shop.owner, line).await.unwrap(),
        Weighed::Refused(WeighRefusal::Overload)
    );
    assert_eq!(
        line_of(&t.carts.current(&t.shop.owner).await.unwrap(), line).0,
        Quantity::from_milli(750)
    );
    // The manual fallback: the quantity is typed.
    let view = t
        .carts
        .set_quantity(&t.shop.owner, line, Quantity::from_milli(1_125))
        .await
        .unwrap();
    assert_eq!(line_of(&view, line).0, Quantity::from_milli(1_125));
    // Or typed for a new line: a line of its own, like a weighing (0.5 kg × 2.99 = 1.495 → 1.50).
    let (view, typed, weight) = done(
        t.carts
            .add_typed_weight(
                &t.shop.owner,
                t.bananas,
                Quantity::from_milli(500),
                AddVia::Search,
            )
            .await
            .unwrap(),
    );
    assert_ne!(typed, line);
    assert_eq!(weight, mg(500_000));
    assert_eq!(line_of(&view, typed).1, Money::from_minor(150));
    assert_eq!(
        line_of(&view, line).0,
        Quantity::from_milli(1_125),
        "the first line is unchanged"
    );
    assert!(
        t.carts
            .add_typed_weight(&t.shop.owner, t.bananas, Quantity::ZERO, AddVia::Search)
            .await
            .is_err()
    );
    assert!(
        t.carts
            .add_typed_weight(&t.shop.owner, t.cola, Quantity::from_milli(1_000), AddVia::Search)
            .await
            .is_err()
    );
    // A line sold by the piece cannot be weighed.
    let (view, _) = t
        .carts
        .add_variant(&t.shop.owner, t.cola, None, AddVia::Search)
        .await
        .unwrap();
    let cola = view.cart.lines.last().unwrap().id;
    assert!(t.carts.weigh_line(&t.shop.owner, cola).await.is_err());
    // A cashier may weigh (it is selling).
    let cashier = t.shop.as_role(pos_domain::Role::Cashier);
    t.device.answer(vec![Ok(ScaleReading::Stable(mg(300_000)))]);
    assert!(matches!(
        t.carts
            .add_weighed(&cashier, t.bananas, AddVia::Search)
            .await
            .unwrap(),
        Weighed::Done { .. }
    ));
}
