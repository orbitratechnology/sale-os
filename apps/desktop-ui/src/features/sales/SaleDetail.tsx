import {
  ArrowLeft,
  ReceiptText,
} from "../../ui/icons";
import { useEffect, useState } from "react";
import { commands, type IpcError } from "../../ipc";
import type { SaleDetailDto_Serialize as Detail } from "../../ipc/bindings";
import { formatMilli } from "../../lib/decimal";
import { formatMoney } from "../../lib/money";
import { PrintControls } from "../receipts/PrintControls";
import { ReceiptPreview } from "../receipts/ReceiptPreview";
import { describeError } from "../shell/errors";
import { REASON_LABEL_COPY, TENDER_LABEL_COPY } from "./labels";
import { useT } from "../../i18n";
import { SaleSyncMark } from "../sync/SaleSyncMark";
import { TaxInvoicePanel } from "./TaxInvoicePanel";

/**
 * One completed sale as recorded (E12-T03): lines, discounts, tax, tenders with who
 * confirmed them, returns made since, the stored receipt, reprint, and "Return items".
 */
export function SaleDetail({
  saleId,
  backLabel,
  onBack,
  onReturn,
}: {
  saleId: string;
  /** Where Esc and the back button go (already translated): the sales list, the stock history, the audit log. */
  backLabel?: string;
  onBack: () => void;
  onReturn: (receiptNumber: string) => void;
}) {
  const t = useT();
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
        <h1 className="page-title">
          <ReceiptText />
          {t("Sale")} <span className="mono">{sale.receiptNumber}</span>
        </h1>
        <button type="button" className="secondary" onClick={onBack}>
          <ArrowLeft />
          {backLabel ?? t("Back to sales")} <kbd>Esc</kbd>
        </button>
      </header>
      <section className="card sale-detail" aria-label={t("Sale detail")}>
        <p>
          {sale.localTime} · {t("served by {name}", { name: sale.cashierName })}
          {sale.approverName ? ` · ${t("approved by {name}", { name: sale.approverName })}` : ""}
          {sale.mode === "sandbox" ? ` · ${t("SANDBOX")}` : ""}
        </p>
        <p className="small" data-testid="sale-sync">
          <SaleSyncMark saleId={sale.saleId} />
        </p>
        <table className="sales-table" aria-label={t("Lines")}>
          <thead>
            <tr>
              <th scope="col">{t("Item")}</th>
              <th scope="col" className="num">
                {t("Qty")}
              </th>
              <th scope="col" className="num">
                {t("Price")}
              </th>
              <th scope="col" className="num">
                {t("Discount")}
              </th>
              <th scope="col" className="num">
                {t("Total")}
              </th>
              <th scope="col" className="num">
                {t("Returned")}
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
        {sale.lines.some((l) => l.priceOverridden) && <p className="small">* {t("price changed at the till")}</p>}

        <dl className="totals" aria-label={t("Totals")}>
          <dt>{t("Subtotal")}</dt>
          <dd>{money(sale.subtotalMinor)}</dd>
          {sale.lineDiscountTotalMinor !== "0" && (
            <>
              <dt>{t("Item discounts")}</dt>
              <dd>-{money(sale.lineDiscountTotalMinor)}</dd>
            </>
          )}
          {sale.orderDiscountTotalMinor !== "0" && (
            <>
              <dt>{t("Order discount")}</dt>
              <dd>-{money(sale.orderDiscountTotalMinor)}</dd>
            </>
          )}
          {sale.taxBreakdown.map((tax) => (
            <Row
              key={tax.name}
              label={sale.taxMode === "inclusive" ? t("{tax} (included)", { tax: tax.name }) : tax.name}
              value={money(tax.taxMinor)}
            />
          ))}
          {sale.roundingMinor !== "0" && <Row label={t("Rounding")} value={money(sale.roundingMinor)} />}
          <dt>
            <strong>{t("Total {currency}", { currency: sale.currencyCode })}</strong>
          </dt>
          <dd data-testid="sale-total">
            <strong>{money(sale.totalMinor)}</strong>
          </dd>
        </dl>

        <h2>{t("Payment")}</h2>
        <ul className="plain-list" aria-label={t("Tenders")}>
          {sale.tenders.map((tender, i) => (
            <li key={i}>
              <strong>{t(TENDER_LABEL_COPY[tender.kind])}</strong> {money(tender.amountMinor)}
              {tender.receivedMinor && tender.kind === "cash" ? ` · ${t("received {amount}", { amount: money(tender.receivedMinor) })}` : ""}
              {tender.changeMinor !== "0" ? ` · ${t("change {amount}", { amount: money(tender.changeMinor) })}` : ""}
              {tender.kind !== "cash" && (
                <span className="small">
                  {" "}
                  ·{" "}
                  {tender.confirmationSource === "provider"
                    ? t("Confirmed by the payment provider")
                    : t("Externally confirmed by operator")}
                </span>
              )}
              {tender.reference && (
                <span className="small">
                  {" "}
                  · {t("ref")} {tender.reference}
                </span>
              )}
              {tender.status !== "confirmed" && <span className="small"> · {tender.status}</span>}
            </li>
          ))}
        </ul>

        <h2>{t("Returns")}</h2>
        {sale.returns.length === 0 ? (
          <p className="small">{t("None.")}</p>
        ) : (
          <ul className="plain-list" aria-label={t("Returns")}>
            {sale.returns.map((r) => (
              <li key={r.returnId}>
                <span className="mono">{r.returnNumber}</span> · {r.localTime} ·{" "}
                {t(REASON_LABEL_COPY[r.reason] ?? r.reason)} ·{" "}
                {t("refunded {amount} ({tenders})", {
                  amount: money(r.totalMinor),
                  tenders: r.refunds.map((k) => t(TENDER_LABEL_COPY[k])).join(" + "),
                })}
              </li>
            ))}
          </ul>
        )}

        <PrintControls source={{ kind: "sale", id: sale.saleId }} autoPrint={false} />
        {showReceipt && <ReceiptPreview source={{ kind: "sale", id: sale.saleId }} />}
        <TaxInvoicePanel saleId={sale.saleId} hasVat={sale.taxTotalMinor !== "0"} />
        <div className="actions">
          <button type="button" className="secondary" onClick={() => setShowReceipt((s) => !s)}>
            {showReceipt ? t("Hide receipt") : t("Show receipt")}
          </button>
          {anyLeft && (
            <button type="button" onClick={() => onReturn(sale.receiptNumber)}>
              {t("Return items")}
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
