import { TabList, tabPanelProps } from "../../ui/Tabs";
import {
  ArrowLeft,
  ChartColumn,
  ClipboardList,
  Inbox,
  Wallet,
} from "../../ui/icons";
import { useEffect, useState } from "react";
import { commands, type IpcError, type Workspace } from "../../ipc";
import type {
  CloudGroupByDto,
  CloudSalesReportDto,
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
import { CloudReport, GROUPS } from "./CloudReport";
import { useT, type T } from "../../i18n";

type Tab = "sales" | "payments" | "top" | "stock";
/** The report tabs, in order; English, translated at render with `t`. */
const TAB_COPY: Record<Tab, string> = {
  sales: "Daily sales",
  payments: "Payments",
  top: "Top products",
  stock: "Stock",
};
const TABS = Object.keys(TAB_COPY) as Tab[];

/** YYYY-MM-DD for a local date (this computer's calendar; the business day in practice). */
function iso(d: Date): string {
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
}

function presets(t: T): { label: string; from: string; to: string }[] {
  const today = new Date();
  const back = (days: number) => {
    const d = new Date(today);
    d.setDate(d.getDate() - days);
    return iso(d);
  };
  return [
    { label: t("Today"), from: iso(today), to: iso(today) },
    { label: t("Yesterday"), from: back(1), to: back(1) },
    { label: t("Last 7 days"), from: back(6), to: iso(today) },
    { label: t("This month"), from: `${iso(today).slice(0, 8)}01`, to: iso(today) },
  ];
}

/**
 * Reports (E14-T03). Every figure comes from Rust and is defined in
 * docs/reporting-definitions.md. Every report says where its numbers come from: this device
 * only, or, once linked, all registers from the cloud (E14-T05), labelled with how complete
 * they are, or stale from the last fetch while the cloud is out of reach.
 */
export function ReportsScreen({ workspace, onBack, onAudit }: { workspace: Workspace; onBack: () => void; onAudit: (() => void) | null }) {
  const t = useT();
  const [tab, setTab] = useState<Tab>("sales");
  const [range, setRange] = useState(() => presets(t)[0] ?? { from: iso(new Date()), to: iso(new Date()) });
  const [topBy, setTopBy] = useState<"quantity" | "value">("value");
  const [lowOnly, setLowOnly] = useState(false);
  const [sales, setSales] = useState<SalesSummaryDto | null>(null);
  const [mix, setMix] = useState<TenderMixDto | null>(null);
  const [top, setTop] = useState<TopProductsDto | null>(null);
  const [stock, setStock] = useState<StockReportDto | null>(null);
  const [error, setError] = useState<IpcError | null>(null);
  const [saved, setSaved] = useState<ExportResultDto | null>(null);
  const [exporting, setExporting] = useState(false);
  const [linked, setLinked] = useState(false);
  const [where, setWhere] = useState<"device" | "cloud">("device");
  const [groupBy, setGroupBy] = useState<CloudGroupByDto>("day");
  const [cloud, setCloud] = useState<CloudSalesReportDto | null>(null);
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
    void commands.syncStatus().then((res) => {
      if (live && res.status === "ok") setLinked(res.data.cloudConfigured && res.data.linkState === "linked");
    });
    return () => {
      live = false;
    };
  }, []);

  useEffect(() => {
    if (where !== "cloud") return;
    let live = true;
    void commands.reportsCloudSales({ from: range.from, to: range.to }, groupBy).then((res) => {
      if (!live) return;
      if (res.status === "ok") {
        setCloud(res.data);
        setError(null);
      } else setError(res.error);
    });
    return () => {
      live = false;
    };
  }, [where, groupBy, range.from, range.to]);

  useEffect(() => {
    if (where === "cloud") return;
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
  }, [where, tab, range.from, range.to, topBy, lowOnly]);

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

  const [savedBefore, savedAfter = ""] = saved
    ? (saved.rows === 1 ? t("Saved 1 row to {path}") : t("Saved {n} rows to {path}", { n: saved.rows })).split("{path}")
    : [];

  const scope = (tab === "sales" ? sales?.scope : tab === "payments" ? mix?.scope : tab === "top" ? top?.scope : stock?.scope) ?? null;

  return (
    <main className="sales">
      <header className="returns-header">
        <h1 className="page-title">
          <ChartColumn />
          {t("Reports")}
        </h1>
        <span className="header-actions">
          {onAudit && (
            <button type="button" className="secondary" onClick={onAudit}>
              <ClipboardList />
              {t("Audit log")}
            </button>
          )}
          <button type="button" className="secondary" onClick={onBack}>
            <ArrowLeft />
            {t("Back to till")} <kbd>Esc</kbd>
          </button>
        </span>
      </header>
      {linked && (
        <div className="receipt-widths" role="group" aria-label={t("Scope")}>
          {(
            [
              ["device", t("This register")],
              ["cloud", t("All registers (cloud)")],
            ] as const
          ).map(([v, label]) => (
            <button
              key={v}
              type="button"
              aria-pressed={where === v}
              className={where === v ? "" : "secondary"}
              onClick={() => {
                setError(null);
                setCloud(null);
                setWhere(v);
              }}
            >
              {label}
            </button>
          ))}
        </div>
      )}
      {where === "cloud" ? (
        <div className="receipt-widths" role="group" aria-label={t("Group by")}>
          {GROUPS.map((g) => (
            <button
              key={g.value}
              type="button"
              aria-pressed={groupBy === g.value}
              className={groupBy === g.value ? "" : "secondary"}
              onClick={() => {
                setCloud(null);
                setGroupBy(g.value);
              }}
            >
              {t(g.label)}
            </button>
          ))}
        </div>
      ) : (
      <TabList
        label={t("Report")}
        idBase="report"
        className="receipt-widths"
        tabs={TABS.map((value) => ({ value, label: t(TAB_COPY[value]) }))}
        selected={tab}
        onSelect={setTab}
      />
      )}
      {(where === "cloud" || tab !== "stock") && (
        <form className="card sales-filters" aria-label={t("Dates")} onSubmit={(e) => e.preventDefault()}>
          <div className="receipt-widths" role="group" aria-label={t("Period")}>
            {presets(t).map((p) => (
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
            {t("From")}
            <input type="date" value={range.from} onChange={(e) => e.target.value && setRange((r) => ({ ...r, from: e.target.value }))} />
          </label>
          <label>
            {t("To")}
            <input type="date" value={range.to} onChange={(e) => e.target.value && setRange((r) => ({ ...r, to: e.target.value }))} />
          </label>
        </form>
      )}
      {where === "device" && (
      <div className="report-bar">
        <ScopeBadge scope={scope} />
        <button type="button" className="secondary" disabled={exporting} onClick={() => void exportCsv(tabDataset[tab])}>
          {t("Export this report (CSV)")}
        </button>
      </div>
      )}
      <div aria-live="polite">
        {saved && (
          <p role="status" className="small" data-testid="export-saved">
            {savedBefore}
            <span className="mono">{saved.path}</span>
            {savedAfter}
          </p>
        )}
      </div>
      {error && (
        <p role="alert" className="field-error">
          {describeError(error)}
        </p>
      )}

      {where === "cloud" ? (
        <section className="card" aria-label={t("All registers")}>
          {cloud ? <CloudReport r={cloud} /> : !error && <p aria-busy="true">{t("Asking the cloud…")}</p>}
        </section>
      ) : (
      <>
      <section className="card report-exports" aria-label={t("Data exports")}>
        <span className="small">{t("Export the records for these dates (CSV):")}</span>
        {(
          [
            ["transactions", t("Sale lines")],
            ["tenders", t("Payments")],
            ["returns", t("Returns")],
            ["products", t("Products")],
          ] as const
        ).map(([d, label]) => (
          <button key={d} type="button" className="secondary" disabled={exporting} onClick={() => void exportCsv(d)}>
            {label}
          </button>
        ))}
      </section>
      <section className="card" {...tabPanelProps("report", tab)}>
        {tab === "sales" && sales && <SalesReport s={sales} money={money} />}
        {tab === "payments" && mix && <PaymentsReport m={mix} money={money} />}
        {tab === "top" && top && (
          <>
            <label>
              {t("Rank by")}{" "}
              <select value={topBy} onChange={(e) => setTopBy(e.target.value as "quantity" | "value")}>
                <option value="value">{t("Value")}</option>
                <option value="quantity">{t("Quantity")}</option>
              </select>
            </label>
            <p className="small">{t("Returns made in the period are taken off.")}</p>
            <table className="sales-table" aria-label={t("Top products")}>
              <thead>
                <tr>
                  <th scope="col">#</th>
                  <th scope="col">{t("Item")}</th>
                  <th scope="col" className="num">
                    {t("Quantity")}
                  </th>
                  <th scope="col" className="num">
                    {t("Value")}
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
            {top.rows.length === 0 && (
        <div className="empty-state">
          <span className="empty-icon">
            <ChartColumn />
          </span>
          <p>{t("No sales in this period.")}</p>
        </div>
      )}
          </>
        )}
        {tab === "stock" && stock && (
          <>
            <p className="drawer-needed" role="note">
              {t("Stock on hand is calculated from recorded movements; it is not a physical count.")}
            </p>
            <label className="check">
              <input type="checkbox" checked={lowOnly} onChange={(e) => setLowOnly(e.target.checked)} /> {t("Low and negative only")}
            </label>
            <table className="sales-table" aria-label={t("Stock on hand")}>
              <thead>
                <tr>
                  <th scope="col">{t("Item")}</th>
                  <th scope="col" className="num">
                    {t("On hand")}
                  </th>
                  <th scope="col" className="num">
                    {t("Damaged")}
                  </th>
                  <th scope="col" className="num">
                    {t("Low at")}
                  </th>
                  <th scope="col">{t("Status")}</th>
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
                      <span className={`stock-status ${r.status}`}>{r.status === "ok" ? t("OK") : r.status === "low" ? t("Low") : t("Negative")}</span>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
            {stock.rows.length === 0 && (
        <div className="empty-state">
          <span className="empty-icon">
            <Inbox />
          </span>
          <p>{t("Nothing to show.")}</p>
        </div>
      )}
          </>
        )}
      </section>
      </>
      )}
    </main>
  );
}

function ScopeBadge({ scope }: { scope: ReportScopeDto | null }) {
  const t = useT();
  if (!scope) return null;
  const at = new Date(parseInt(scope.asOfMs, 10)); // a timestamp, not money
  const time = `${String(at.getHours()).padStart(2, "0")}:${String(at.getMinutes()).padStart(2, "0")}`;
  return (
    <p className="scope-badge" data-testid="scope-badge">
      {scope.scope === "device"
        ? t("This device only · as of {time}", { time })
        : t("{scope} · as of {time}", { scope: scope.scope, time })}
    </p>
  );
}

function SalesReport({ s, money }: { s: SalesSummaryDto; money: (m: string) => string }) {
  const t = useT();
  const tot = s.total;
  return (
    <>
      <dl className="totals report-totals" aria-label={t("Totals for the period")}>
        <Row label={s.pricesIncludeTax ? t("Gross sales (incl. tax)") : t("Gross sales (excl. tax)")} value={money(tot.grossMinor)} testId="gross" />
        <Row label={t("Line discounts")} value={neg(tot.lineDiscountsMinor, money)} />
        <Row label={t("Order discounts")} value={neg(tot.orderDiscountsMinor, money)} />
        <Row label={t("Discounts")} value={neg(tot.discountsMinor, money)} testId="discounts" />
        <Row label={t("Tax")} value={money(tot.taxMinor)} testId="tax" />
        {tot.roundingMinor !== "0" && <Row label={t("Cash rounding")} value={money(tot.roundingMinor)} />}
        <Row label={t("Sales ({n})", { n: tot.salesCount })} value={money(tot.salesTotalMinor)} testId="sales-total" />
        <Row label={t("Refunds ({n} returns)", { n: tot.returnsCount })} value={neg(tot.refundsMinor, money)} testId="refunds" />
        <Row label={t("Net sales (incl. tax)")} value={money(tot.netInclTaxMinor)} testId="net-incl" strong />
        <Row label={t("Net sales (excl. tax)")} value={money(tot.netExclTaxMinor)} testId="net-excl" strong />
      </dl>
      <table className="sales-table" aria-label={t("By day")}>
        <thead>
          <tr>
            <th scope="col">{t("Day")}</th>
            <th scope="col" className="num">
              {t("Sales")}
            </th>
            <th scope="col" className="num">
              {t("Discounts")}
            </th>
            <th scope="col" className="num">
              {t("Tax")}
            </th>
            <th scope="col" className="num">
              {t("Refunds")}
            </th>
            <th scope="col" className="num">
              {t("Net (incl. tax)")}
            </th>
          </tr>
        </thead>
        <tbody>
          {s.days.map((d) => (
            <DayRow key={d.date} date={d.date} f={d.figures} money={money} />
          ))}
        </tbody>
      </table>
      {s.days.length === 0 && (
        <div className="empty-state">
          <span className="empty-icon">
            <ChartColumn />
          </span>
          <p>{t("No sales in this period.")}</p>
        </div>
      )}
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
  const t = useT();
  return (
    <>
      <table className="sales-table" aria-label={t("Payment mix")}>
        <thead>
          <tr>
            <th scope="col">{t("Payment")}</th>
            <th scope="col">{t("Confirmed by")}</th>
            <th scope="col" className="num">
              {t("Taken")}
            </th>
            <th scope="col" className="num">
              {t("Refunded")}
            </th>
            <th scope="col" className="num">
              {t("Net")}
            </th>
          </tr>
        </thead>
        <tbody>
          {m.rows.map((r) => (
            <tr key={`${r.kind}-${r.confirmationSource}`} data-testid="mix-row">
              <th scope="row">{t(TENDER_LABEL[r.kind])}</th>
              <td>{r.confirmationSource === "provider" ? t("Payment provider") : t("Recorded by staff")}</td>
              <td className="num">
                {money(r.takenMinor)} ({r.count})
              </td>
              <td className="num">{r.refundCount ? `${neg(r.refundedMinor, money)} (${r.refundCount})` : ""}</td>
              <td className="num">{money(r.netMinor)}</td>
            </tr>
          ))}
        </tbody>
      </table>
      {m.rows.length === 0 && (
        <div className="empty-state">
          <span className="empty-icon">
            <Wallet />
          </span>
          <p>{t("No payments in this period.")}</p>
        </div>
      )}
      <p className="small">{t("Cash is what was kept: handed over less change.")}</p>
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
