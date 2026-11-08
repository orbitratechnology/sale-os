import { useEffect, useState } from "react";
import { commands, type IpcError, type Workspace } from "../../ipc";
import type {
  ExportDatasetDto,
  ExportResultDto,
  ReportScopeDto,
  SalesFiguresDto,
  SalesSummaryDto,
  StockReportDto,
  TenderMixDto,
  TopProductsDto,
} from "../../ipc/bindings";
import { formatMilli } from "../../lib/decimal";
import { formatMoney } from "../../lib/money";
import { TENDER_LABEL } from "../sales/labels";
import { describeError } from "../shell/errors";

type Tab = "sales" | "payments" | "top" | "stock";
const TABS: { value: Tab; label: string }[] = [
  { value: "sales", label: "Daily sales" },
  { value: "payments", label: "Payments" },
  { value: "top", label: "Top products" },
  { value: "stock", label: "Stock" },
];

/** YYYY-MM-DD for a local date (this computer's calendar; the business day in practice). */
function iso(d: Date): string {
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
}

function presets(): { label: string; from: string; to: string }[] {
  const today = new Date();
  const back = (days: number) => {
    const d = new Date(today);
    d.setDate(d.getDate() - days);
    return iso(d);
  };
  return [
    { label: "Today", from: iso(today), to: iso(today) },
    { label: "Yesterday", from: back(1), to: back(1) },
    { label: "Last 7 days", from: back(6), to: iso(today) },
    { label: "This month", from: `${iso(today).slice(0, 8)}01`, to: iso(today) },
  ];
}

/**
 * Reports (E14-T03). Every figure comes from Rust and is defined in
 * docs/reporting-definitions.md. Every report says where its numbers come from: in Stage 2
 * that is always this device only.
 */
export function ReportsScreen({ workspace, onBack, onAudit }: { workspace: Workspace; onBack: () => void; onAudit: (() => void) | null }) {
  const [tab, setTab] = useState<Tab>("sales");
  const [range, setRange] = useState(() => presets()[0] ?? { from: iso(new Date()), to: iso(new Date()) });
  const [topBy, setTopBy] = useState<"quantity" | "value">("value");
  const [lowOnly, setLowOnly] = useState(false);
  const [sales, setSales] = useState<SalesSummaryDto | null>(null);
  const [mix, setMix] = useState<TenderMixDto | null>(null);
  const [top, setTop] = useState<TopProductsDto | null>(null);
  const [stock, setStock] = useState<StockReportDto | null>(null);
  const [error, setError] = useState<IpcError | null>(null);
  const [saved, setSaved] = useState<ExportResultDto | null>(null);
  const [exporting, setExporting] = useState(false);
  const exp = sales?.currencyExponent ?? workspace.currencyExponent;
  const money = (m: string) => formatMoney(m, { exponent: exp });

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      onBack();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onBack]);

  useEffect(() => {
    let live = true;
    const r = { from: range.from, to: range.to };
    const done = <T,>(set: (v: T) => void) => (res: { status: "ok"; data: T } | { status: "error"; error: IpcError }) => {
      if (!live) return;
      if (res.status === "ok") {
        set(res.data);
        setError(null);
      } else setError(res.error);
    };
    if (tab === "sales") void commands.reportsSalesSummary(r).then(done(setSales));
    if (tab === "payments") void commands.reportsTenderMix(r).then(done(setMix));
    if (tab === "top") void commands.reportsTopProducts(r, topBy, 20).then(done(setTop));
    if (tab === "stock") void commands.reportsStock(lowOnly).then(done(setStock));
    return () => {
      live = false;
    };
  }, [tab, range.from, range.to, topBy, lowOnly]);

  async function exportCsv(dataset: ExportDatasetDto) {
    setExporting(true);
    setSaved(null);
    const dated = dataset !== "products" && dataset !== "stock";
    const res = await commands.exportCsv(dataset, dated ? { from: range.from, to: range.to } : null);
    setExporting(false);
    if (res.status === "ok") {
      // null: the save dialog was cancelled; nothing was written.
      setSaved(res.data);
      setError(null);
    } else setError(res.error);
  }
  const tabDataset: Record<Tab, ExportDatasetDto> = { sales: "sales_summary", payments: "tender_mix", top: "top_products", stock: "stock" };

  const scope = (tab === "sales" ? sales?.scope : tab === "payments" ? mix?.scope : tab === "top" ? top?.scope : stock?.scope) ?? null;

  return (
    <main className="sales">
      <header className="returns-header">
        <h1>Reports</h1>
        <span>
          {onAudit && (
            <button type="button" className="secondary" onClick={onAudit}>
              Audit log
            </button>
          )}{" "}
          <button type="button" className="secondary" onClick={onBack}>
            Back to till <kbd>Esc</kbd>
          </button>
        </span>
      </header>
      <div className="receipt-widths" role="tablist" aria-label="Report">
        {TABS.map((t) => (
          <button key={t.value} type="button" role="tab" aria-selected={tab === t.value} className={tab === t.value ? "" : "secondary"} onClick={() => setTab(t.value)}>
            {t.label}
          </button>
        ))}
      </div>
      {tab !== "stock" && (
        <form className="card sales-filters" aria-label="Dates" onSubmit={(e) => e.preventDefault()}>
          <div className="receipt-widths" role="group" aria-label="Period">
            {presets().map((p) => (
              <button
                key={p.label}
                type="button"
                className={range.from === p.from && range.to === p.to ? "" : "secondary"}
                onClick={() => setRange({ from: p.from, to: p.to })}
              >
                {p.label}
              </button>
            ))}
          </div>
          <label>
            From
            <input type="date" value={range.from} onChange={(e) => e.target.value && setRange((r) => ({ ...r, from: e.target.value }))} />
          </label>
          <label>
            To
            <input type="date" value={range.to} onChange={(e) => e.target.value && setRange((r) => ({ ...r, to: e.target.value }))} />
          </label>
        </form>
      )}
      <div className="report-bar">
        <ScopeBadge scope={scope} />
        <button type="button" className="secondary" disabled={exporting} onClick={() => void exportCsv(tabDataset[tab])}>
          Export this report (CSV)
        </button>
      </div>
      <div aria-live="polite">
        {saved && (
          <p role="status" className="small" data-testid="export-saved">
            Saved {saved.rows} {saved.rows === 1 ? "row" : "rows"} to <span className="mono">{saved.path}</span>
          </p>
        )}
      </div>
      {error && (
        <p role="alert" className="field-error">
          {describeError(error)}
        </p>
      )}

      <section className="card report-exports" aria-label="Data exports">
        <span className="small">Export the records for these dates (CSV):</span>
        {(
          [
            ["transactions", "Sale lines"],
            ["tenders", "Payments"],
            ["returns", "Returns"],
            ["products", "Products"],
          ] as const
        ).map(([d, label]) => (
          <button key={d} type="button" className="secondary" disabled={exporting} onClick={() => void exportCsv(d)}>
            {label}
          </button>
        ))}
      </section>
      <section className="card" aria-label={TABS.find((t) => t.value === tab)?.label}>
        {tab === "sales" && sales && <SalesReport s={sales} money={money} />}
        {tab === "payments" && mix && <PaymentsReport m={mix} money={money} />}
        {tab === "top" && top && (
          <>
            <label>
              Rank by{" "}
              <select value={topBy} onChange={(e) => setTopBy(e.target.value as "quantity" | "value")}>
                <option value="value">Value</option>
                <option value="quantity">Quantity</option>
              </select>
            </label>
            <p className="small">Returns made in the period are taken off.</p>
            <table className="sales-table" aria-label="Top products">
              <thead>
                <tr>
                  <th scope="col">#</th>
                  <th scope="col">Item</th>
                  <th scope="col" className="num">
                    Quantity
                  </th>
                  <th scope="col" className="num">
                    Value
                  </th>
                </tr>
              </thead>
              <tbody>
                {top.rows.map((r, i) => (
                  <tr key={r.variantId} data-testid="top-row">
                    <td>{i + 1}</td>
                    <th scope="row">{r.label ? `${r.name} ${r.label}` : r.name}</th>
                    <td className="num">{formatMilli(r.quantityMilli)}</td>
                    <td className="num">{money(r.valueMinor)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
            {top.rows.length === 0 && <p className="small">No sales in this period.</p>}
          </>
        )}
        {tab === "stock" && stock && (
          <>
            <p className="drawer-needed" role="note">
              Stock on hand is calculated from recorded movements; it is not a physical count.
            </p>
            <label>
              <input type="checkbox" checked={lowOnly} onChange={(e) => setLowOnly(e.target.checked)} /> Low and negative only
            </label>
            <table className="sales-table" aria-label="Stock on hand">
              <thead>
                <tr>
                  <th scope="col">Item</th>
                  <th scope="col" className="num">
                    On hand
                  </th>
                  <th scope="col" className="num">
                    Damaged
                  </th>
                  <th scope="col" className="num">
                    Low at
                  </th>
                  <th scope="col">Status</th>
                </tr>
              </thead>
              <tbody>
                {stock.rows.map((r) => (
                  <tr key={r.variantId} data-testid="stock-row">
                    <th scope="row">
                      {r.label ? `${r.name} ${r.label}` : r.name}
                      {r.sku && <span className="small"> · {r.sku}</span>}
                    </th>
                    <td className="num">{formatMilli(r.sellableMilli)}</td>
                    <td className="num">{r.damagedMilli === "0" ? "" : formatMilli(r.damagedMilli)}</td>
                    <td className="num">{r.lowStockThresholdMilli ? formatMilli(r.lowStockThresholdMilli) : ""}</td>
                    <td>
                      <span className={`stock-status ${r.status}`}>{r.status === "ok" ? "OK" : r.status === "low" ? "Low" : "Negative"}</span>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
            {stock.rows.length === 0 && <p className="small">Nothing to show.</p>}
          </>
        )}
      </section>
    </main>
  );
}

function ScopeBadge({ scope }: { scope: ReportScopeDto | null }) {
  if (!scope) return null;
  const at = new Date(parseInt(scope.asOfMs, 10)); // a timestamp, not money
  const time = `${String(at.getHours()).padStart(2, "0")}:${String(at.getMinutes()).padStart(2, "0")}`;
  return (
    <p className="scope-badge" data-testid="scope-badge">
      {scope.scope === "device" ? "This device only" : scope.scope} · as of {time}
    </p>
  );
}

function SalesReport({ s, money }: { s: SalesSummaryDto; money: (m: string) => string }) {
  const t = s.total;
  return (
    <>
      <dl className="totals report-totals" aria-label="Totals for the period">
        <Row label={`Gross sales${s.pricesIncludeTax ? " (incl. tax)" : " (excl. tax)"}`} value={money(t.grossMinor)} testId="gross" />
        <Row label="Line discounts" value={neg(t.lineDiscountsMinor, money)} />
        <Row label="Order discounts" value={neg(t.orderDiscountsMinor, money)} />
        <Row label="Discounts" value={neg(t.discountsMinor, money)} testId="discounts" />
        <Row label="Tax" value={money(t.taxMinor)} testId="tax" />
        {t.roundingMinor !== "0" && <Row label="Cash rounding" value={money(t.roundingMinor)} />}
        <Row label={`Sales (${t.salesCount})`} value={money(t.salesTotalMinor)} testId="sales-total" />
        <Row label={`Refunds (${t.returnsCount} returns)`} value={neg(t.refundsMinor, money)} testId="refunds" />
        <Row label="Net sales (incl. tax)" value={money(t.netInclTaxMinor)} testId="net-incl" strong />
        <Row label="Net sales (excl. tax)" value={money(t.netExclTaxMinor)} testId="net-excl" strong />
      </dl>
      <table className="sales-table" aria-label="By day">
        <thead>
          <tr>
            <th scope="col">Day</th>
            <th scope="col" className="num">
              Sales
            </th>
            <th scope="col" className="num">
              Discounts
            </th>
            <th scope="col" className="num">
              Tax
            </th>
            <th scope="col" className="num">
              Refunds
            </th>
            <th scope="col" className="num">
              Net (incl. tax)
            </th>
          </tr>
        </thead>
        <tbody>
          {s.days.map((d) => (
            <DayRow key={d.date} date={d.date} f={d.figures} money={money} />
          ))}
        </tbody>
      </table>
      {s.days.length === 0 && <p className="small">No sales in this period.</p>}
    </>
  );
}

function DayRow({ date, f, money }: { date: string; f: SalesFiguresDto; money: (m: string) => string }) {
  return (
    <tr data-testid="day-row">
      <th scope="row">{date}</th>
      <td className="num">
        {money(f.salesTotalMinor)} ({f.salesCount})
      </td>
      <td className="num">{neg(f.discountsMinor, money)}</td>
      <td className="num">{money(f.taxMinor)}</td>
      <td className="num">{neg(f.refundsMinor, money)}</td>
      <td className="num">{money(f.netInclTaxMinor)}</td>
    </tr>
  );
}

function PaymentsReport({ m, money }: { m: TenderMixDto; money: (x: string) => string }) {
  return (
    <>
      <table className="sales-table" aria-label="Payment mix">
        <thead>
          <tr>
            <th scope="col">Payment</th>
            <th scope="col">Confirmed by</th>
            <th scope="col" className="num">
              Taken
            </th>
            <th scope="col" className="num">
              Refunded
            </th>
            <th scope="col" className="num">
              Net
            </th>
          </tr>
        </thead>
        <tbody>
          {m.rows.map((r) => (
            <tr key={`${r.kind}-${r.confirmationSource}`} data-testid="mix-row">
              <th scope="row">{TENDER_LABEL[r.kind]}</th>
              <td>{r.confirmationSource === "provider" ? "Payment provider" : "Recorded by staff"}</td>
              <td className="num">
                {money(r.takenMinor)} ({r.count})
              </td>
              <td className="num">{r.refundCount ? `${neg(r.refundedMinor, money)} (${r.refundCount})` : ""}</td>
              <td className="num">{money(r.netMinor)}</td>
            </tr>
          ))}
        </tbody>
      </table>
      {m.rows.length === 0 && <p className="small">No payments in this period.</p>}
      <p className="small">Cash is what was kept: handed over less change.</p>
    </>
  );
}

function neg(minor: string, money: (m: string) => string): string {
  return minor === "0" ? money("0") : `-${money(minor)}`;
}

function Row({ label, value, testId, strong }: { label: string; value: string; testId?: string; strong?: boolean }) {
  return (
    <>
      <dt>{strong ? <strong>{label}</strong> : label}</dt>
      <dd data-testid={testId}>{strong ? <strong>{value}</strong> : value}</dd>
    </>
  );
}
