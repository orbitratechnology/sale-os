# ADR-004: Rounding policy (the single documented policy, PRD §9.3)

- **Status:** Proposed — needs acceptance by the tech lead and one other engineer (E00-T03)
- **Date:** 2026-11-05
- **Source:** [docs/backlog/03-decisions.md](../backlog/03-decisions.md), PRD §8–§14

## Context

The PRD (docs/PRD.md) fixes the product constraints this decision serves: offline-first local
checkout, atomic local transactions, at-least-once sync without duplicate effects, decimal-safe
money, and hardware independence. This ADR records the engineering choice the backlog assumes.

## Decision

- **Rounding mode:** half away from zero ("commercial rounding").
- **Line level:**
  - `gross = unit_price × quantity`, rounded to minor units.
  - Line discounts are applied to the rounded gross.
  - Order discounts are **allocated to lines pro rata by line net-before-order-discount**,
    using the **largest-remainder method**. The allocation always sums exactly to the order
    discount.
  - **Tax is calculated and rounded per line, per tax rate.**
- **Document level:** totals are sums of rounded line values. The document level never
  re-rounds.
- **Tax-inclusive prices:** `tax = round(net_incl × rate / (1 + rate))`, and
  `net_excl = net_incl − tax`. The customer-facing line total equals the shelf price × qty
  minus discounts.
- **Cash rounding** (for example to 0.05) is optional per business. It applies only when
  the tender is entirely cash. It is recorded as a separate `rounding_minor` on the sale, not
  folded into lines.
- **Partial returns** refund `round(line_total × cumulative_returned_qty / line_qty)` minus
  what was already refunded for that line. This makes a full return refund exactly the line
  total.
- If the D-02 pilot country requires per-invoice tax rounding, implement it as a second
  `TaxRoundingMode` variant. Do not change this default.

## Consequences

- Changing this after Stage 0 requires a new ADR that supersedes this one and an impact review of the tasks that cite it.
- Tasks citing this ADR in `docs/backlog/tasks/` must be re-checked if it is superseded.
