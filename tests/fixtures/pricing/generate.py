#!/usr/bin/env python3
"""Pricing golden scenarios (E08-T03) and an independent reference of ADR-004.

The expected values in this folder are NOT produced by the Rust code they test. This script
computes them with exact rational arithmetic (`fractions.Fraction`), straight from the rules
in docs/adr/0004-rounding-policy-*.md:

- round half away from zero (or half to even when a scenario says so), once per step;
- gross = round(unit price x quantity), or the label's price;
- line discount on the rounded gross; order discount allocated pro rata by the line net
  before it, largest remainder, ties to the lowest index;
- tax per line, per rate: exclusive round(net x r); inclusive T = round(net x R / (1 + R)),
  split across the rates by largest remainder on their rates;
- totals are sums of rounded lines, never re-rounded;
- cash rounding only when every tender is cash, kept apart from the lines;
- partial returns refund round(total x returned_after / qty) - round(total x returned_before / qty).

Run `python3 tests/fixtures/pricing/generate.py` after changing a scenario, and review the diff.
A rounding change must update these files in the same PR (CODEOWNERS). Two test runners read
the JSON: `crates/pos-domain/tests/pricing_goldens.rs` (Rust computes every amount) and
`apps/desktop-ui/src/lib/pricingGoldens.test.ts` (the UI formats them; it does no maths).
"""

from __future__ import annotations

import json
from fractions import Fraction
from pathlib import Path

HERE = Path(__file__).resolve().parent
PPM = 1_000_000


# ------------------------------------------------------------------ arithmetic ---


def rnd(x: Fraction, mode: str) -> int:
    if mode == "half_even":
        return round(x)  # Python rounds a Fraction half to even
    sign = -1 if x < 0 else 1
    a = abs(x)
    whole = a.numerator // a.denominator
    if a - whole >= Fraction(1, 2):
        whole += 1
    return sign * whole


def allocate(total: int, weights: list[int]) -> list[int]:
    if not weights:
        assert total == 0
        return []
    s = sum(weights)
    eff = weights if s else [1] * len(weights)
    den = s if s else len(weights)
    mag = abs(total)
    parts, rems = [], []
    for i, w in enumerate(eff):
        share = mag * w
        parts.append(share // den)
        rems.append((share % den, i))
    left = mag - sum(parts)
    rems.sort(key=lambda r: (-r[0], r[1]))
    for _, i in rems[:left]:
        parts[i] += 1
    return [-p for p in parts] if total < 0 else parts


def qty(s: str) -> Fraction:
    return Fraction(s)


def fmt(minor: int, exponent: int) -> str:
    """en-US style: '-1,234.56'. What both the Rust and the UI formatter must print."""
    neg = minor < 0
    digits = str(abs(minor)).rjust(exponent + 1, "0")
    whole, frac = (digits[:-exponent], digits[-exponent:]) if exponent else (digits, "")
    groups = []
    while len(whole) > 3:
        groups.insert(0, whole[-3:])
        whole = whole[:-3]
    groups.insert(0, whole)
    out = ",".join(groups) + ("." + frac if exponent else "")
    return ("-" if neg and minor != 0 else "") + out


class Invalid(Exception):
    def __init__(self, code: str):
        self.code = code


# ---------------------------------------------------------------- the reference ---


def price(sc: dict) -> dict:
    mode = sc.get("rounding", "half_away_from_zero")
    rates = sc.get("rates", {})
    lines = sc["lines"]

    def discount(base: int, d: dict | None) -> int:
        if d is None:
            return 0
        if "percentPpm" in d:
            return rnd(Fraction(base * d["percentPpm"], PPM), mode)
        return int(d["fixed"])

    gross, ldisc, net1 = [], [], []
    for l in lines:
        q = qty(l["quantity"])
        if q <= 0:
            raise Invalid("VALIDATION_FAILED")
        if int(l["unitPrice"]) < 0:
            raise Invalid("VALIDATION_FAILED")
        g = int(l["labelGross"]) if "labelGross" in l else rnd(int(l["unitPrice"]) * q, mode)
        d = discount(g, l.get("discount"))
        if d < 0:
            raise Invalid("VALIDATION_FAILED")
        if d > g:
            raise Invalid("PRICING_DISCOUNT_EXCEEDS")
        gross.append(g)
        ldisc.append(d)
        net1.append(g - d)

    order = discount(sum(net1), sc.get("orderDiscount"))
    if order < 0 or order > sum(net1):
        raise Invalid("PRICING_DISCOUNT_EXCEEDS")
    alloc = allocate(order, net1)

    out_lines, breakdown = [], {}
    for i, l in enumerate(lines):
        net2 = net1[i] - alloc[i]
        keys = l.get("taxes", [])
        tax_lines = []
        if not keys:
            tax, total = 0, net2
        elif sc["taxMode"] == "exclusive":
            for k in keys:
                tax_lines.append({"rate": k, "taxable": net2, "tax": rnd(Fraction(net2 * rates[k]["ppm"], PPM), mode)})
            tax = sum(t["tax"] for t in tax_lines)
            total = net2 + tax
        else:
            combined = Fraction(sum(rates[k]["ppm"] for k in keys), PPM)
            tax = 0 if combined == 0 else rnd(net2 * combined / (1 + combined), mode)
            split = allocate(tax, [rates[k]["ppm"] for k in keys])
            tax_lines = [{"rate": k, "taxable": net2 - tax, "tax": s} for k, s in zip(keys, split)]
            total = net2
        for t in tax_lines:
            b = breakdown.setdefault(t["rate"], {"rate": t["rate"], "taxable": 0, "tax": 0})
            b["taxable"] += t["taxable"]
            b["tax"] += t["tax"]
        out_lines.append(
            {
                "id": l["id"],
                "gross": gross[i],
                "lineDiscount": ldisc[i],
                "orderDiscount": alloc[i],
                "taxLines": tax_lines,
                "tax": tax,
                "total": total,
            }
        )

    result = {
        "lines": out_lines,
        "subtotal": sum(gross),
        "lineDiscountTotal": sum(ldisc),
        "orderDiscountTotal": order,
        "taxTotal": sum(l["tax"] for l in out_lines),
        "taxBreakdown": list(breakdown.values()),
        "total": sum(l["total"] for l in out_lines),
    }
    if "tenders" in sc:
        result["settlement"] = settle(result["total"], sc, mode)
    return result


def settle(total: int, sc: dict, mode: str) -> dict:
    tenders = sc["tenders"]
    inc = int(sc.get("cashIncrement", "0"))
    all_cash = all(t["kind"] == "cash" for t in tenders)
    rounding = rnd(Fraction(total, inc), mode) * inc - total if all_cash and inc > 0 else 0
    due = total + rounding
    remaining, change = due, 0
    for i, t in enumerate(tenders):
        amount = int(t["amount"])
        last = i == len(tenders) - 1
        if t["kind"] == "cash":
            applied = min(amount, remaining)
            change = amount - applied
            assert change == 0 or last, "change only from the last cash tender"
        else:
            assert amount <= remaining, "a non-cash tender cannot exceed what is due"
            applied = amount
        remaining -= applied
    if remaining > 0:
        raise Invalid("TENDER_INSUFFICIENT")
    return {"rounding": rounding, "totalDue": due, "change": change}


def returns(sc: dict) -> list[dict]:
    line = sc["sold"]
    q = qty(line["quantity"])
    total, tax = int(line["total"]), int(line["tax"])
    done = Fraction(0)
    out = []
    for r in sc["returns"]:
        after = done + qty(r["quantity"])
        assert after <= q
        refund = rnd(total * after / q, "half_away_from_zero") - rnd(total * done / q, "half_away_from_zero")
        t = rnd(tax * after / q, "half_away_from_zero") - rnd(tax * done / q, "half_away_from_zero")
        out.append({"quantity": r["quantity"], "refund": refund, "tax": t})
        done = after
    return out


# ---------------------------------------------------------------------- output ---


def strs(v):
    """Amounts travel as strings of minor units, as they do over IPC."""
    if isinstance(v, bool):
        return v
    if isinstance(v, int):
        return str(v)
    if isinstance(v, list):
        return [strs(x) for x in v]
    if isinstance(v, dict):
        return {k: (x if k in ("id", "rate", "quantity") else strs(x)) for k, x in v.items()}
    return v


def display(expect: dict, exponent: int) -> dict:
    d = {}
    for k in ("subtotal", "lineDiscountTotal", "orderDiscountTotal", "taxTotal", "total"):
        if k in expect:
            d[k] = fmt(int(expect[k]), exponent)
    if "lines" in expect:
        d["lineTotals"] = [fmt(int(l["total"]), exponent) for l in expect["lines"]]
    if "settlement" in expect:
        for k, v in expect["settlement"].items():
            d[k] = fmt(int(v), exponent)
    if "returns" in expect:
        d["refunds"] = [fmt(int(r["refund"]), exponent) for r in expect["returns"]]
    return d


def build(sc: dict) -> dict:
    sc = dict(sc)
    sc.setdefault("currency", {"code": "USD", "exponent": 2})
    if sc.get("kind") == "return":
        expect = {"returns": returns(sc)}
    else:
        sc.setdefault("kind", "cart")
        try:
            expect = price(sc)
        except Invalid as e:
            sc["expectError"] = e.code
            return sc
    expect = strs(expect)
    expect["display"] = display(expect, sc["currency"]["exponent"])
    sc["expect"] = expect
    return sc


# -------------------------------------------------------------------- scenarios ---

USD = {"code": "USD", "exponent": 2}
VAT20 = {"vat20": {"name": "VAT 20%", "ppm": 200_000}}


def L(id, price, q="1", **kw):
    line = {"id": id, "unitPrice": str(price), "quantity": q}
    line.update(kw)
    return line


GROUPS: dict[str, tuple[str, list[dict]]] = {}

GROUPS["01-line-gross"] = (
    "ADR-004 line level: gross = round(unit price x quantity) to minor units, half away from zero; a label price is the gross as printed.",
    [
        {"id": "gross-integer-quantity", "description": "9.99 x 3, no tax", "taxMode": "exclusive",
         "lines": [L("a", 999, "3")]},
        {"id": "gross-weighed-rounds-up", "description": "0.333 kg x 12.49 = 4.15917 -> 4.16", "taxMode": "exclusive",
         "lines": [L("a", 1249, "0.333")]},
        {"id": "gross-midpoint-away-from-zero", "description": "2.5 minor units rounds to 3", "taxMode": "exclusive",
         "lines": [L("a", 5, "0.5")]},
        {"id": "gross-midpoint-half-even", "description": "the same line under half-to-even rounds to 2", "taxMode": "exclusive",
         "rounding": "half_even", "lines": [L("a", 5, "0.5")]},
        {"id": "gross-fractional-quantity", "description": "1.235 x 1.99 = 2.45765 -> 2.46", "taxMode": "exclusive",
         "lines": [L("a", 199, "1.235")]},
        {"id": "gross-tiny-quantity-rounds-to-zero", "description": "0.001 x 0.01 is less than half a cent", "taxMode": "exclusive",
         "lines": [L("a", 1, "0.001")]},
        {"id": "gross-label-price", "description": "a scale label's price is the gross, not 0.734 kg x 12.50 = 9.175", "taxMode": "inclusive",
         "rates": VAT20, "lines": [L("a", 1250, "0.734", labelGross="399", taxes=["vat20"])]},
        {"id": "gross-beyond-2-53", "description": "an amount above 2^53 stays exact end to end (the UI must not use JS numbers)",
         "taxMode": "exclusive", "lines": [L("a", 9007199254740993, "1")]},
        {"id": "gross-zero-price", "description": "a free item is a line of zero", "taxMode": "inclusive", "rates": VAT20,
         "lines": [L("a", 0, "2", taxes=["vat20"]), L("b", 500, "1", taxes=["vat20"])]},
    ],
)

GROUPS["02-line-discounts"] = (
    "ADR-004: a line discount applies to the rounded gross; a percentage is rounded once.",
    [
        {"id": "line-percent-rounds", "description": "10% of 9.99 = 0.999 -> 1.00", "taxMode": "exclusive",
         "lines": [L("a", 999, discount={"percentPpm": 100_000})]},
        {"id": "line-percent-below-half", "description": "12.5% of 0.01 = 0.00125 -> 0", "taxMode": "exclusive",
         "lines": [L("a", 1, discount={"percentPpm": 125_000})]},
        {"id": "line-percent-midpoint", "description": "15% of 0.30 = 0.045 -> 0.05", "taxMode": "exclusive",
         "lines": [L("a", 30, discount={"percentPpm": 150_000})]},
        {"id": "line-percent-on-rounded-gross", "description": "the percentage is of the rounded gross 4.16, not 4.15917",
         "taxMode": "exclusive", "lines": [L("a", 1249, "0.333", discount={"percentPpm": 250_000})]},
        {"id": "line-fixed", "description": "0.50 off 9.99", "taxMode": "exclusive",
         "lines": [L("a", 999, discount={"fixed": "50"})]},
        {"id": "line-hundred-percent", "description": "a free line: no tax either", "taxMode": "inclusive", "rates": VAT20,
         "lines": [L("a", 999, discount={"percentPpm": 1_000_000}, taxes=["vat20"])]},
        {"id": "line-fixed-equals-gross", "description": "a fixed discount may take the line to zero", "taxMode": "exclusive",
         "lines": [L("a", 250, "2", discount={"fixed": "500"})]},
        {"id": "line-fixed-exceeds-gross", "description": "refused: 5.01 off a 5.00 line", "taxMode": "exclusive",
         "lines": [L("a", 250, "2", discount={"fixed": "501"})]},
    ],
)

GROUPS["03-order-discount"] = (
    "ADR-004: the order discount is allocated to lines pro rata by their net before it, largest remainder, ties to the first line; the allocation sums exactly.",
    [
        {"id": "order-percent-exact-split", "description": "10% of 60.00 over 10/20/30", "taxMode": "exclusive",
         "lines": [L("a", 1000), L("b", 2000), L("c", 3000)], "orderDiscount": {"percentPpm": 100_000}},
        {"id": "order-fixed-tie-to-first", "description": "1.00 over three equal lines: 0.34/0.33/0.33", "taxMode": "exclusive",
         "lines": [L("a", 1000), L("b", 1000), L("c", 1000)], "orderDiscount": {"fixed": "100"}},
        {"id": "order-largest-remainder-wins", "description": "0.02 over 0.01 and 0.02: shares 0.67 and 1.33, the larger remainder gets the cent",
         "taxMode": "exclusive", "lines": [L("a", 1), L("b", 2)], "orderDiscount": {"fixed": "2"}},
        {"id": "order-after-line-discounts", "description": "weights are the nets after line discounts", "taxMode": "exclusive",
         "lines": [L("a", 1000, discount={"percentPpm": 500_000}), L("b", 1000)], "orderDiscount": {"fixed": "300"}},
        {"id": "order-skips-free-line", "description": "a line discounted to zero takes no share", "taxMode": "exclusive",
         "lines": [L("a", 700, discount={"percentPpm": 1_000_000}), L("b", 300), L("c", 600)], "orderDiscount": {"fixed": "100"}},
        {"id": "order-percent-midpoint", "description": "5% of 10.10 = 0.505 -> 0.51", "taxMode": "exclusive",
         "lines": [L("a", 1010)], "orderDiscount": {"percentPpm": 50_000}},
        {"id": "order-fixed-whole-cart", "description": "the whole cart free", "taxMode": "inclusive", "rates": VAT20,
         "lines": [L("a", 333, taxes=["vat20"]), L("b", 667, taxes=["vat20"])], "orderDiscount": {"fixed": "1000"}},
        {"id": "order-exceeds-cart", "description": "refused: 10.01 off 10.00", "taxMode": "exclusive",
         "lines": [L("a", 1000)], "orderDiscount": {"fixed": "1001"}},
        {"id": "order-many-lines-sums-exactly", "description": "1.00 over seven lines of 1.43", "taxMode": "exclusive",
         "lines": [L(c, 143) for c in "abcdefg"], "orderDiscount": {"fixed": "100"}},
    ],
)

GROUPS["04-tax-inclusive"] = (
    "ADR-004: prices include tax; per line, per rate, tax = round(net x R / (1 + R)); several rates share that tax by their rates; the customer pays the shelf price.",
    [
        {"id": "incl-15-three-items", "description": "9.99 x 3 at 15%: tax 3.91", "taxMode": "inclusive",
         "rates": {"gst15": {"name": "GST 15%", "ppm": 150_000}}, "lines": [L("a", 999, "3", taxes=["gst15"])]},
        {"id": "incl-20-after-discounts", "description": "10% line and 1.00 order discount, then 20% inclusive", "taxMode": "inclusive",
         "rates": VAT20, "lines": [L("a", 999, discount={"percentPpm": 100_000}, taxes=["vat20"])], "orderDiscount": {"fixed": "99"}},
        {"id": "incl-midpoint", "description": "0.63 at 20%: 0.105 -> 0.11", "taxMode": "inclusive", "rates": VAT20,
         "lines": [L("a", 63, taxes=["vat20"])]},
        {"id": "incl-two-rates-split", "description": "10% + 5% on 10.00: tax 1.30 split 0.87/0.43", "taxMode": "inclusive",
         "rates": {"state10": {"name": "State 10%", "ppm": 100_000}, "city5": {"name": "City 5%", "ppm": 50_000}},
         "lines": [L("a", 1000, taxes=["state10", "city5"])]},
        {"id": "incl-zero-rated", "description": "a 0% rate: a tax line of zero, unlike an exempt item", "taxMode": "inclusive",
         "rates": {"zero": {"name": "Zero-rated", "ppm": 0}}, "lines": [L("a", 450, taxes=["zero"])]},
        {"id": "incl-exempt", "description": "no rate at all: no tax line", "taxMode": "inclusive", "lines": [L("a", 450)]},
        {"id": "incl-per-line-not-per-document", "description": "three lines of 0.10 at 20%: 0.02 each, 0.06 in all (a document-level round would give 0.05)",
         "taxMode": "inclusive", "rates": VAT20, "lines": [L(c, 10, taxes=["vat20"]) for c in "abc"]},
        {"id": "incl-mixed-rates-breakdown", "description": "two rates over three lines: the breakdown sums per rate, in first-seen order",
         "taxMode": "inclusive", "rates": {"vat20": VAT20["vat20"], "red5": {"name": "Reduced 5%", "ppm": 50_000}},
         "lines": [L("a", 1299, taxes=["red5"]), L("b", 2500, taxes=["vat20"]), L("c", 349, "3", taxes=["red5"])]},
        {"id": "incl-sri-lanka-vat18", "description": "Sri Lanka VAT 18% on LKR 1,500.00 and LKR 249.50 x 2",
         "currency": {"code": "LKR", "exponent": 2}, "taxMode": "inclusive",
         "rates": {"vat18": {"name": "VAT 18%", "ppm": 180_000}},
         "lines": [L("a", 150000, taxes=["vat18"]), L("b", 24950, "2", taxes=["vat18"])]},
    ],
)

GROUPS["05-tax-exclusive"] = (
    "ADR-004: tax added on top; per line, per rate, tax = round(net x r).",
    [
        {"id": "excl-7-25-weighed", "description": "0.333 kg x 12.49 at 7.25%: 4.16 + 0.30", "taxMode": "exclusive",
         "rates": {"sales": {"name": "Sales tax 7.25%", "ppm": 72_500}}, "lines": [L("a", 1249, "0.333", taxes=["sales"])]},
        {"id": "excl-two-rates-each-on-net", "description": "5% and 2.5% each on 10.00", "taxMode": "exclusive",
         "rates": {"state5": {"name": "State 5%", "ppm": 50_000}, "local2_5": {"name": "Local 2.5%", "ppm": 25_000}},
         "lines": [L("a", 1000, taxes=["state5", "local2_5"])]},
        {"id": "excl-midpoint", "description": "10% of 0.15 = 0.015 -> 0.02", "taxMode": "exclusive",
         "rates": {"t10": {"name": "Tax 10%", "ppm": 100_000}}, "lines": [L("a", 15, taxes=["t10"])]},
        {"id": "excl-midpoint-half-even", "description": "the same under half-to-even: 0.02 (1.5 -> 2)", "taxMode": "exclusive",
         "rounding": "half_even", "rates": {"t10": {"name": "Tax 10%", "ppm": 100_000}}, "lines": [L("a", 15, taxes=["t10"])]},
        {"id": "excl-after-order-discount", "description": "tax on the net after the order discount share", "taxMode": "exclusive",
         "rates": {"t8": {"name": "Tax 8%", "ppm": 80_000}},
         "lines": [L("a", 1999, taxes=["t8"]), L("b", 501, taxes=["t8"])], "orderDiscount": {"percentPpm": 150_000}},
        {"id": "excl-per-line-not-per-document", "description": "two lines of 0.05 at 10%: 0.01 each (a document-level round would give 0.01 in all)",
         "taxMode": "exclusive", "rates": {"t10": {"name": "Tax 10%", "ppm": 100_000}},
         "lines": [L("a", 5, taxes=["t10"]), L("b", 5, taxes=["t10"])]},
        {"id": "excl-taxed-and-exempt", "description": "a taxed and an exempt line", "taxMode": "exclusive",
         "rates": {"t8": {"name": "Tax 8%", "ppm": 80_000}}, "lines": [L("a", 1250, taxes=["t8"]), L("b", 300, "2")]},
    ],
)

GROUPS["06-cash-rounding"] = (
    "ADR-004: optional cash rounding to an increment, only when every tender is cash, recorded apart from the lines.",
    [
        {"id": "cash-round-down", "description": "1.02 to 0.05: due 1.00, change 1.00 from 2.00", "taxMode": "exclusive",
         "cashIncrement": "5", "lines": [L("a", 102)], "tenders": [{"kind": "cash", "amount": "200"}]},
        {"id": "cash-round-up", "description": "1.03 to 0.05: due 1.05", "taxMode": "exclusive",
         "cashIncrement": "5", "lines": [L("a", 103)], "tenders": [{"kind": "cash", "amount": "105"}]},
        {"id": "cash-round-midpoint", "description": "1.05 to 0.10: midpoint goes up to 1.10", "taxMode": "exclusive",
         "cashIncrement": "10", "lines": [L("a", 105)], "tenders": [{"kind": "cash", "amount": "110"}]},
        {"id": "cash-round-midpoint-half-even", "description": "1.05 to 0.10 under half-to-even: 1.00", "taxMode": "exclusive",
         "rounding": "half_even", "cashIncrement": "10", "lines": [L("a", 105)], "tenders": [{"kind": "cash", "amount": "100"}]},
        {"id": "cash-round-not-with-card", "description": "card plus cash: no rounding", "taxMode": "exclusive",
         "cashIncrement": "5", "lines": [L("a", 1002)], "tenders": [{"kind": "card_external", "amount": "500"}, {"kind": "cash", "amount": "600"}]},
        {"id": "cash-round-off", "description": "no increment set: no rounding", "taxMode": "exclusive",
         "lines": [L("a", 1002)], "tenders": [{"kind": "cash", "amount": "2000"}]},
        {"id": "cash-round-lkr-fifty-cents", "description": "LKR 123.75 to 0.50: 124.00", "currency": {"code": "LKR", "exponent": 2},
         "taxMode": "inclusive", "rates": {"vat18": {"name": "VAT 18%", "ppm": 180_000}}, "cashIncrement": "50",
         "lines": [L("a", 12375, taxes=["vat18"])], "tenders": [{"kind": "cash", "amount": "15000"}]},
        {"id": "cash-round-lines-unchanged", "description": "the rounding is not folded into the lines or the tax", "taxMode": "inclusive",
         "rates": VAT20, "cashIncrement": "5", "lines": [L("a", 199, taxes=["vat20"]), L("b", 99, "2", taxes=["vat20"])],
         "tenders": [{"kind": "cash", "amount": "500"}]},
        {"id": "cash-insufficient", "description": "refused: 0.95 for 1.00 due", "taxMode": "exclusive",
         "cashIncrement": "5", "lines": [L("a", 99)], "tenders": [{"kind": "cash", "amount": "95"}]},
    ],
)

GROUPS["07-currencies"] = (
    "Minor units follow the currency exponent; the same rules apply with 0 and 3 decimals.",
    [
        {"id": "jpy-no-decimals", "description": "JPY: 10% inclusive on 1,980", "currency": {"code": "JPY", "exponent": 0},
         "taxMode": "inclusive", "rates": {"c10": {"name": "Consumption 10%", "ppm": 100_000}},
         "lines": [L("a", 1980, taxes=["c10"]), L("b", 108, "3", taxes=["c10"])]},
        {"id": "kwd-three-decimals", "description": "KWD: 5% exclusive on 1.250 x 3", "currency": {"code": "KWD", "exponent": 3},
         "taxMode": "exclusive", "rates": {"v5": {"name": "VAT 5%", "ppm": 50_000}}, "lines": [L("a", 1250, "3", taxes=["v5"])]},
        {"id": "large-amount-grouping", "description": "LKR 1,234,567.89 with thousands separators", "currency": {"code": "LKR", "exponent": 2},
         "taxMode": "exclusive", "lines": [L("a", 123456789)]},
    ],
)

GROUPS["08-partial-returns"] = (
    "ADR-004: partial returns refund round(line total x returned after / qty) minus what was already refunded, so a line's refunds add up to exactly its total. Tax follows the same rule.",
    [
        {"id": "return-thirds", "description": "10.00 (tax 1.67) of 3, returned one by one: 3.33, 3.34, 3.33",
         "kind": "return", "sold": {"quantity": "3", "total": "1000", "tax": "167"},
         "returns": [{"quantity": "1"}, {"quantity": "1"}, {"quantity": "1"}]},
        {"id": "return-whole-at-once", "description": "all 3 at once refunds exactly the line", "kind": "return",
         "sold": {"quantity": "3", "total": "1000", "tax": "167"}, "returns": [{"quantity": "3"}]},
        {"id": "return-weighed", "description": "0.750 kg for 9.37, returned 0.250 then 0.500", "kind": "return",
         "sold": {"quantity": "0.750", "total": "937", "tax": "156"}, "returns": [{"quantity": "0.250"}, {"quantity": "0.500"}]},
        {"id": "return-discounted-line", "description": "a line that paid 8.01 after discounts, 7 sold, returned 2 then 5", "kind": "return",
         "sold": {"quantity": "7", "total": "801", "tax": "0"}, "returns": [{"quantity": "2"}, {"quantity": "5"}]},
    ],
)


def main() -> None:
    count = 0
    for name, (rule, scenarios) in GROUPS.items():
        doc = {
            "rule": rule,
            "generatedBy": "tests/fixtures/pricing/generate.py (do not edit by hand)",
            "scenarios": [build(s) for s in scenarios],
        }
        count += len(scenarios)
        (HERE / f"{name}.json").write_text(json.dumps(doc, indent=2, ensure_ascii=False) + "\n")
    print(f"wrote {len(GROUPS)} files, {count} scenarios")


if __name__ == "__main__":
    main()
