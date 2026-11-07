import { useEffect, useState } from "react";
import { commands, type IpcError } from "../../ipc";
import type { SaleDetailDto_Serialize as Detail } from "../../ipc/bindings";
import { formatMilli } from "../../lib/decimal";
import { formatMoney } from "../../lib/money";
import { PrintControls } from "../receipts/PrintControls";
import { ReceiptPreview } from "../receipts/ReceiptPreview";
import { describeError } from "../shell/errors";
import { REASON_LABEL, TENDER_LABEL } from "./labels";

/**
 * One completed sale as recorded (E12-T03): lines, discounts, tax, tenders with who
 * confirmed them, returns made since, the stored receipt, reprint, and "Return items".
 */
export function SaleDetail({
  saleId,
  onBack,
  onReturn,
}: {
  saleId: string;
  onBack: () => void;
  onReturn: (receiptNumber: string) => void;
}) {
  const [sale, setSale] = useState<Detail | null>(null);
  const [error, setError] = useState<IpcError | null>(null);
  const [showReceipt, setShowReceipt] = useState(false);

  useEffect(() => {
    let live = true;
    void commands.salesGet(saleId).then((res) => {
      if (!live) return;
      if (res.status === "ok") setSale(res.data);
      else setError(res.error);
    });
    return () => {
      live = false;
    };
  }, [saleId]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      onBack();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onBack]);

  if (error) return <p role="alert">{describeError(error)}</p>;
  if (!sale) return <main className="sales" aria-busy="true" />;
  const money = (minor: string) => formatMoney(minor, { exponent: sale.currencyExponent });
  const anyLeft = sale.lines.some((l) => BigInt(l.returnedMilli) < BigInt(l.quantityMilli));

  return (
    <main className="sales">
      <header className="returns-header">
        <h1>
          Sale <span className="mono">{sale.receiptNumber}</span>
        </h1>
        <button type="button" className="secondary" onClick={onBack}>
          Back to sales <kbd>Esc</kbd>
        </button>
      </header>
      <section className="card sale-detail" aria-label="Sale detail">
        <p>
          {sale.localTime} · served by {sale.cashierName}
          {sale.approverName ? ` · approved by ${sale.approverName}` : ""}
          {sale.mode === "sandbox" ? " · SANDBOX" : ""}
        </p>
        <table className="sales-table" aria-label="Lines">
          <thead>
            <tr>
              <th scope="col">Item</th>
              <th scope="col" className="num">
                Qty
              </th>
              <th scope="col" className="num">
                Price
              </th>
              <th scope="col" className="num">
                Discount
              </th>
              <th scope="col" className="num">
                Total
              </th>
              <th scope="col" className="num">
                Returned
              </th>
            </tr>
          </thead>
          <tbody>
            {sale.lines.map((l) => (
              <tr key={l.lineNo}>
                <th scope="row">
                  {l.label ? `${l.name} ${l.label}` : l.name}
                  {l.sku && <span className="small"> · {l.sku}</span>}
                </th>
                <td className="num">{formatMilli(l.quantityMilli)}</td>
                <td className="num">
                  {money(l.unitPriceMinor)}
                  {l.priceOverridden ? " *" : ""}
                </td>
                <td className="num">{l.discountMinor === "0" ? "" : `-${money(l.discountMinor)}`}</td>
                <td className="num">{money(l.totalMinor)}</td>
                <td className="num">{l.returnedMilli === "0" ? "" : formatMilli(l.returnedMilli)}</td>
              </tr>
            ))}
          </tbody>
        </table>
        {sale.lines.some((l) => l.priceOverridden) && <p className="small">* price changed at the till</p>}

        <dl className="totals" aria-label="Totals">
          <dt>Subtotal</dt>
          <dd>{money(sale.subtotalMinor)}</dd>
          {sale.lineDiscountTotalMinor !== "0" && (
            <>
              <dt>Item discounts</dt>
              <dd>-{money(sale.lineDiscountTotalMinor)}</dd>
            </>
          )}
          {sale.orderDiscountTotalMinor !== "0" && (
            <>
              <dt>Order discount</dt>
              <dd>-{money(sale.orderDiscountTotalMinor)}</dd>
            </>
          )}
          {sale.taxBreakdown.map((t) => (
            <Row
              key={t.name}
              label={`${t.name}${sale.taxMode === "inclusive" ? " (included)" : ""}`}
              value={money(t.taxMinor)}
            />
          ))}
          {sale.roundingMinor !== "0" && <Row label="Rounding" value={money(sale.roundingMinor)} />}
          <dt>
            <strong>Total {sale.currencyCode}</strong>
          </dt>
          <dd data-testid="sale-total">
            <strong>{money(sale.totalMinor)}</strong>
          </dd>
        </dl>

        <h2>Payment</h2>
        <ul className="plain-list" aria-label="Tenders">
          {sale.tenders.map((t, i) => (
            <li key={i}>
              <strong>{TENDER_LABEL[t.kind]}</strong> {money(t.amountMinor)}
              {t.receivedMinor && t.kind === "cash" ? ` · received ${money(t.receivedMinor)}` : ""}
              {t.changeMinor !== "0" ? ` · change ${money(t.changeMinor)}` : ""}
              {t.kind !== "cash" && (
                <span className="small">
                  {" "}
                  · {t.confirmationSource === "provider" ? "Confirmed by the payment provider" : "Externally confirmed by operator"}
                </span>
              )}
              {t.reference && <span className="small"> · ref {t.reference}</span>}
              {t.status !== "confirmed" && <span className="small"> · {t.status}</span>}
            </li>
          ))}
        </ul>

        <h2>Returns</h2>
        {sale.returns.length === 0 ? (
          <p className="small">None.</p>
        ) : (
          <ul className="plain-list" aria-label="Returns">
            {sale.returns.map((r) => (
              <li key={r.returnId}>
                <span className="mono">{r.returnNumber}</span> · {r.localTime} · {REASON_LABEL[r.reason] ?? r.reason} · refunded{" "}
                {money(r.totalMinor)} ({r.refunds.map((k) => TENDER_LABEL[k]).join(" + ")})
              </li>
            ))}
          </ul>
        )}

        <PrintControls source={{ kind: "sale", id: sale.saleId }} autoPrint={false} />
        {showReceipt && <ReceiptPreview source={{ kind: "sale", id: sale.saleId }} />}
        <div className="actions">
          <button type="button" className="secondary" onClick={() => setShowReceipt((s) => !s)}>
            {showReceipt ? "Hide receipt" : "Show receipt"}
          </button>
          {anyLeft && (
            <button type="button" onClick={() => onReturn(sale.receiptNumber)}>
              Return items
            </button>
          )}
        </div>
      </section>
    </main>
  );
}

function Row({ label, value }: { label: string; value: string }) {
  return (
    <>
      <dt>{label}</dt>
      <dd>{value}</dd>
    </>
  );
}
