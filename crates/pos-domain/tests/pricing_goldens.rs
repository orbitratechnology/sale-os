//! E08-T03: the pricing golden scenarios in `tests/fixtures/pricing/*.json`.
//!
//! The expected values come from an independent reference of ADR-004 (exact fractions, in
//! `tests/fixtures/pricing/generate.py`), not from this crate. This runner checks that
//! `price_cart`, `settle` and `plan_return` produce every amount, and that the domain's
//! formatter prints them as the fixture's `display` strings (the UI checks the same strings
//! in `apps/desktop-ui/src/lib/pricingGoldens.test.ts`).

use pos_domain::catalog::Unit;
use pos_domain::pricing::{
    Discount, PricedCart, PricingLine, PricingPolicy, TaxMode, TaxRateSnapshot, price_cart,
};
use pos_domain::returns::{Disposition, ReturnLineRequest, ReturnableLine, ReturnableSale, plan_return};
use pos_domain::sale::SaleMode;
use pos_domain::settlement::{Settlement, TenderInput, TenderKind, settle};
use pos_domain::{
    CartLineId, Currency, CurrencyCode, HasErrorCode, Money, NumberFormat, ProductId, Quantity, RatePpm,
    RoundingMode, SaleId, SaleLineId, TaxRateId, UuidV7Generator, VariantId,
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;

fn fixtures() -> Vec<(String, Value)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/pricing");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    let mut out = Vec::new();
    for f in files {
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&f).unwrap()).unwrap();
        for s in doc["scenarios"].as_array().unwrap() {
            out.push((s["id"].as_str().unwrap().to_owned(), s.clone()));
        }
    }
    out
}

fn money(v: &Value) -> Money {
    Money::from_minor(
        v.as_str()
            .unwrap_or_else(|| panic!("amount {v} is not a string"))
            .parse()
            .unwrap(),
    )
}

fn s(m: Money) -> String {
    m.minor().to_string()
}

fn quantity(v: &Value) -> Quantity {
    Quantity::parse_decimal(v.as_str().unwrap()).unwrap()
}

fn discount(v: &Value) -> Option<Discount> {
    if v.is_null() {
        return None;
    }
    if let Some(p) = v.get("percentPpm") {
        return Some(Discount::percent(
            RatePpm::new(u32::try_from(p.as_u64().unwrap()).unwrap()).unwrap(),
        ));
    }
    Some(Discount::fixed(money(&v["fixed"])))
}

fn currency(sc: &Value) -> Currency {
    let c = &sc["currency"];
    Currency::new(
        CurrencyCode::new(c["code"].as_str().unwrap()).unwrap(),
        u8::try_from(c["exponent"].as_u64().unwrap()).unwrap(),
    )
    .unwrap()
}

fn policy(sc: &Value) -> PricingPolicy {
    let mut p = PricingPolicy::new(match sc["taxMode"].as_str().unwrap() {
        "inclusive" => TaxMode::Inclusive,
        "exclusive" => TaxMode::Exclusive,
        other => panic!("tax mode {other}"),
    });
    p.rounding = match sc["rounding"].as_str() {
        None | Some("half_away_from_zero") => RoundingMode::HalfAwayFromZero,
        Some("half_even") => RoundingMode::HalfEven,
        Some(other) => panic!("rounding {other}"),
    };
    p.cash_rounding_increment = sc.get("cashIncrement").map(money);
    p
}

fn tender_kind(k: &str) -> TenderKind {
    match k {
        "cash" => TenderKind::Cash,
        "card_external" => TenderKind::CardExternal,
        "bank_transfer_external" => TenderKind::BankTransferExternal,
        "qr_external" => TenderKind::QrExternal,
        "other_external" => TenderKind::OtherExternal,
        other => panic!("tender kind {other}"),
    }
}

/// A priced scenario: the cart, its settlement (when it has tenders) and each rate's fixture key.
type CartRun = (PricedCart, Option<Settlement>, BTreeMap<TaxRateId, String>);

/// Runs one cart scenario. An error is the refusal's wire code.
fn run_cart(sc: &Value) -> Result<CartRun, String> {
    let ids = UuidV7Generator;
    let mut rates: BTreeMap<String, TaxRateSnapshot> = BTreeMap::new();
    if let Some(obj) = sc["rates"].as_object() {
        for (key, r) in obj {
            rates.insert(
                key.clone(),
                TaxRateSnapshot {
                    id: TaxRateId::generate(&ids),
                    name: r["name"].as_str().unwrap().to_owned(),
                    rate: RatePpm::new(u32::try_from(r["ppm"].as_u64().unwrap()).unwrap()).unwrap(),
                },
            );
        }
    }
    let keys = rates.iter().map(|(k, r)| (r.id, k.clone())).collect();
    let lines: Vec<PricingLine> = sc["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| PricingLine {
            line_id: CartLineId::generate(&ids),
            quantity: quantity(&l["quantity"]),
            unit_price: money(&l["unitPrice"]),
            line_discount: discount(&l["discount"]),
            taxes: l["taxes"]
                .as_array()
                .map(|t| t.iter().map(|k| rates[k.as_str().unwrap()].clone()).collect())
                .unwrap_or_default(),
            fixed_gross: l.get("labelGross").map(money),
        })
        .collect();
    let policy = policy(sc);
    let order = discount(&sc["orderDiscount"]);
    let priced =
        price_cart(&lines, order.as_ref(), &policy).map_err(|e| e.error_code().as_str().to_owned())?;
    let settlement = match sc["tenders"].as_array() {
        None => None,
        Some(ts) => {
            let tenders: Vec<TenderInput> = ts
                .iter()
                .map(|t| TenderInput {
                    kind: tender_kind(t["kind"].as_str().unwrap()),
                    amount: money(&t["amount"]),
                    external_reference: None,
                })
                .collect();
            Some(settle(priced.total, &tenders, &policy).map_err(|e| e.error_code().as_str().to_owned())?)
        }
    };
    Ok((priced, settlement, keys))
}

/// The actual results in the fixture's shape, so a mismatch shows as a JSON diff.
fn actual_cart(
    sc: &Value,
    priced: &PricedCart,
    settlement: Option<&Settlement>,
    keys: &BTreeMap<TaxRateId, String>,
) -> Value {
    let tax_lines = |ts: &[pos_domain::pricing::TaxLine]| -> Value {
        ts.iter()
            .map(|t| serde_json::json!({"rate": keys[&t.tax_rate_id], "taxable": s(t.taxable), "tax": s(t.tax)}))
            .collect()
    };
    let lines: Vec<Value> = priced
        .lines
        .iter()
        .zip(sc["lines"].as_array().unwrap())
        .map(|(l, input)| {
            serde_json::json!({
                "id": input["id"],
                "gross": s(l.gross),
                "lineDiscount": s(l.line_discount),
                "orderDiscount": s(l.order_discount_alloc),
                "taxLines": tax_lines(&l.tax_lines),
                "tax": s(l.tax),
                "total": s(l.total),
            })
        })
        .collect();
    let mut v = serde_json::json!({
        "lines": lines,
        "subtotal": s(priced.subtotal),
        "lineDiscountTotal": s(priced.line_discount_total),
        "orderDiscountTotal": s(priced.order_discount_total),
        "taxTotal": s(priced.tax_total),
        "taxBreakdown": tax_lines(&priced.tax_breakdown),
        "total": s(priced.total),
    });
    if let Some(st) = settlement {
        v["settlement"] = serde_json::json!({
            "rounding": s(st.rounding),
            "totalDue": s(st.total_due),
            "change": s(st.change),
        });
    }
    v
}

fn actual_returns(sc: &Value) -> Value {
    let ids = UuidV7Generator;
    let sold = &sc["sold"];
    let line_id = SaleLineId::generate(&ids);
    let qty = quantity(&sold["quantity"]);
    let mut line = ReturnableLine {
        sale_line_id: line_id,
        line_no: 1,
        product_id: ProductId::generate(&ids),
        variant_id: VariantId::generate(&ids),
        name: "Item".into(),
        variant_label: String::new(),
        unit: if qty.milli() % 1000 == 0 {
            Unit::Ea
        } else {
            Unit::Kg
        },
        track_stock: true,
        quantity: qty,
        total: money(&sold["total"]),
        tax: money(&sold["tax"]),
        unit_cost: None,
        returned: Quantity::from_milli(0),
    };
    let mut out = Vec::new();
    for r in sc["returns"].as_array().unwrap() {
        let q = quantity(&r["quantity"]);
        let sale = ReturnableSale {
            sale_id: SaleId::generate(&ids),
            receipt_number: "R01-000001".into(),
            mode: SaleMode::Live,
            currency: currency(sc),
            lines: vec![line.clone()],
        };
        let plan = plan_return(
            &sale,
            SaleMode::Live,
            &[ReturnLineRequest {
                sale_line_id: line_id,
                quantity: q,
                disposition: Disposition::Restock,
            }],
        )
        .unwrap();
        out.push(serde_json::json!({
            "quantity": r["quantity"],
            "refund": s(plan.total),
            "tax": s(plan.tax),
        }));
        line.returned = Quantity::from_milli(line.returned.milli() + q.milli());
    }
    serde_json::json!({ "returns": out })
}

/// The fixture's display strings, printed by the domain's formatter.
fn actual_display(sc: &Value, expect: &Value) -> Value {
    let cur = currency(sc);
    let f = |v: &Value| Value::String(cur.format(money(v), NumberFormat::EN_US));
    let mut d = serde_json::Map::new();
    for k in [
        "subtotal",
        "lineDiscountTotal",
        "orderDiscountTotal",
        "taxTotal",
        "total",
    ] {
        if let Some(v) = expect.get(k) {
            d.insert(k.into(), f(v));
        }
    }
    if let Some(lines) = expect["lines"].as_array() {
        d.insert(
            "lineTotals".into(),
            lines.iter().map(|l| f(&l["total"])).collect(),
        );
    }
    if let Some(st) = expect["settlement"].as_object() {
        for (k, v) in st {
            d.insert(k.clone(), f(v));
        }
    }
    if let Some(rs) = expect["returns"].as_array() {
        d.insert("refunds".into(), rs.iter().map(|r| f(&r["refund"])).collect());
    }
    Value::Object(d)
}

#[test]
fn every_pricing_golden_scenario_matches_the_reference() {
    let all = fixtures();
    assert!(
        all.len() >= 40,
        "E08-T03 asks for at least 40 scenarios, found {}",
        all.len()
    );
    let mut seen = std::collections::BTreeSet::new();
    let mut failures = Vec::new();
    for (id, sc) in &all {
        assert!(seen.insert(id.clone()), "scenario id {id} is used twice");
        if sc["kind"] == "return" {
            let mut actual = actual_returns(sc);
            let mut expect = sc["expect"].clone();
            let display = expect.as_object_mut().unwrap().remove("display").unwrap();
            if actual != expect {
                failures.push(format!("{id}:\n  expected {expect}\n  actual   {actual}"));
                continue;
            }
            actual["display"] = actual_display(sc, &expect);
            if actual["display"] != display {
                failures.push(format!(
                    "{id} display:\n  expected {display}\n  actual   {}",
                    actual["display"]
                ));
            }
            continue;
        }
        match (run_cart(sc), sc.get("expectError")) {
            (Err(code), Some(want)) if want == &Value::String(code.clone()) => {}
            (Err(code), want) => failures.push(format!("{id}: error {code}, expected {want:?}")),
            (Ok(_), Some(want)) => failures.push(format!("{id}: priced, expected error {want}")),
            (Ok((priced, settlement, keys)), None) => {
                let mut expect = sc["expect"].clone();
                let display = expect.as_object_mut().unwrap().remove("display").unwrap();
                let actual = actual_cart(sc, &priced, settlement.as_ref(), &keys);
                if actual != expect {
                    failures.push(format!("{id}:\n  expected {expect}\n  actual   {actual}"));
                    continue;
                }
                let shown = actual_display(sc, &expect);
                if shown != display {
                    failures.push(format!("{id} display:\n  expected {display}\n  actual   {shown}"));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} scenarios differ:\n{}",
        failures.len(),
        all.len(),
        failures.join("\n")
    );
}
