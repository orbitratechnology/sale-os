# Reporting definitions (E14-T01, PRD §15)

Every report figure is defined here, and the queries in
`crates/pos-local-db/src/report_queries.rs` implement exactly this. All amounts are integer
minor units; nothing is converted to floating point.

## Scope

- **This device only** (Stage 2). Reports read the local database: the sales, returns and
  stock movements recorded on this till. Stage 3 adds "Cloud — last synced HH:MM" and
  "Fully synchronized".
- **Live only.** Practice (sandbox) sales and returns are excluded from every report.
- **Days are business days.** A sale belongs to the `business_date` it was given when it
  was made, in the business's time zone (a sale at 23:30 in Kolkata is on the Kolkata date,
  whatever the UTC date). A range `from..to` is inclusive.
- Sales are counted on their own business date; returns and refunds on **the return's**
  business date (not the original sale's).

## Sales summary (per day and for the range)

| Measure | Definition |
|---|---|
| Sales | Number of completed live sales |
| Gross sales | Σ `sales.subtotal_minor` = Σ of each line's quantity × unit price charged, before discounts. Tax-inclusive when prices include tax, tax-exclusive when they do not (the till's tax mode is shown with the report) |
| Line discounts | Σ `sales.line_discount_total_minor` |
| Order discounts | Σ `sales.order_discount_total_minor` |
| Discounts | Line discounts + order discounts |
| Tax | Σ `sales.tax_total_minor`: the tax captured on each sale snapshot |
| Cash rounding | Σ `sales.rounding_minor` (cash rounding, ADR-004; not a sale) |
| Sales total | Σ `sales.total_minor`: what customers were charged, including tax and rounding |
| Returns | Number of live returns |
| Refunds | Σ `refunds.amount_minor` (what was paid back, including tax) |
| Refunded tax | Σ `returns.tax_minor` |
| **Net sales (incl. tax)** | Sales total − cash rounding − refunds |
| **Net sales (excl. tax)** | Net sales (incl. tax) − (tax − refunded tax) |

So, with prices including tax: gross − discounts = sales total − rounding; with prices
excluding tax: gross − discounts + tax = sales total − rounding.

## Payment mix (tender totals)

Per tender type, for sales in the range: the number of tenders and Σ `tenders.amount_minor`
(the amount **applied to the sale**: for cash, what was handed over less change). Confirmed
tenders only. Each type is split by who confirmed it: **recorded by the operator**
(card on a separate terminal, transfer, QR, cash) or **confirmed by a payment provider**
(none until payment integrations exist). Refunds per type are listed separately for returns
in the range, and **net** = taken − refunded.

## Top products

Per variant, for the range: quantity sold = Σ `sale_lines.quantity_milli` of sales in the
range, **less** Σ `return_lines.quantity_milli` of returns in the range; value = Σ
`sale_lines.total_minor` (after discounts, as charged) less Σ `return_lines.refund_minor`.
Returns are netted in the period they happen, so a return of an item sold last month lowers
this period's figures. Ranked by net quantity or net value; ties by name. The name is the
name on the most recent sale line (sale lines keep the name at the time of sale).

## Stock

From the balance projection (`stock_balances`), i.e. **calculated from recorded
movements, not a physical count** (PRD §6.6). Per stock-tracked variant: sellable on hand,
damaged on hand, low-stock threshold, and status:

- **Negative**: sellable < 0;
- **Low**: a threshold is set and sellable ≤ threshold;
- **OK**: otherwise.

Archived products are left out. The low-stock report is the Low and Negative rows.

## Gross margin

Not shown (REP-007, P1): the cost snapshot on sale lines exists, but there is no valuation
policy yet.
