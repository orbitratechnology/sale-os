import { useEffect, useState } from "react";
import { commands, type IpcError } from "../../ipc";
import type { StockHistoryDto, StockReportDto } from "../../ipc/bindings";
import { formatMilli } from "../../lib/decimal";
import { describeError } from "../shell/errors";

const MOVEMENT: Record<string, string> = {
  opening: "Opening stock",
  receipt: "Goods received",
  sale: "Sale",
  return_restock: "Return, back to stock",
  return_damaged: "Return, damaged",
  damage: "Damaged",
  loss: "Lost / theft / expired",
  adjustment: "Adjustment",
  write_off: "Written off",
  correction: "Count correction",
};

/**
 * Stock on hand (E11-T05): every stock-tracked item with sellable and damaged quantities, its
 * low-stock level and status, and its movement history. Figures come from the recorded
 * movements, which the banner says plainly (PRD §6.6).
 */
export function StockOverview() {
  const [lowOnly, setLowOnly] = useState(false);
  const [report, setReport] = useState<StockReportDto | null>(null);
  const [open, setOpen] = useState<{ variantId: string; name: string } | null>(null);
  const [history, setHistory] = useState<StockHistoryDto[] | null>(null);
  const [error, setError] = useState<IpcError | null>(null);

  useEffect(() => {
    let live = true;
    void commands.stockOverview(lowOnly).then((res) => {
      if (!live) return;
      if (res.status === "ok") setReport(res.data);
      else setError(res.error);
    });
    return () => {
      live = false;
    };
  }, [lowOnly]);

  useEffect(() => {
    if (!open) return;
    let live = true;
    void commands.stockHistory(open.variantId, 100).then((res) => {
      if (!live) return;
      if (res.status === "ok") setHistory(res.data);
      else setError(res.error);
    });
    return () => {
      live = false;
    };
  }, [open]);

  return (
    <section className="card return-form" aria-label="Stock on hand">
      <p className="drawer-needed" role="note">
        Stock on hand is calculated from recorded movements; it is not a physical count.
      </p>
      <label>
        <input type="checkbox" checked={lowOnly} onChange={(e) => setLowOnly(e.target.checked)} /> Low and negative only
      </label>
      {error && (
        <p role="alert" className="field-error">
          {describeError(error)}
        </p>
      )}
      <table className="sales-table" aria-label="On hand">
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
            <th scope="col">Last movement</th>
          </tr>
        </thead>
        <tbody>
          {report?.rows.map((r) => {
            const name = r.label ? `${r.name} ${r.label}` : r.name;
            return (
              <tr key={r.variantId} data-testid="onhand-row">
                <th scope="row">
                  <button
                    type="button"
                    className="link"
                    aria-expanded={open?.variantId === r.variantId}
                    onClick={() => {
                      setHistory(null);
                      setOpen(open?.variantId === r.variantId ? null : { variantId: r.variantId, name });
                    }}
                  >
                    {name}
                  </button>
                  {r.sku && <span className="small"> · {r.sku}</span>}
                </th>
                <td className="num">{formatMilli(r.sellableMilli)}</td>
                <td className="num">{r.damagedMilli === "0" ? "" : formatMilli(r.damagedMilli)}</td>
                <td className="num">{r.lowStockThresholdMilli ? formatMilli(r.lowStockThresholdMilli) : ""}</td>
                <td>
                  <span className={`stock-status ${r.status}`}>{r.status === "ok" ? "OK" : r.status === "low" ? "Low" : "Negative"}</span>
                </td>
                <td className="small">{r.lastMovementMs ? when(r.lastMovementMs) : "never"}</td>
              </tr>
            );
          })}
        </tbody>
      </table>
      {report && report.rows.length === 0 && <p className="small">Nothing to show.</p>}

      {open && (
        <div aria-label={`History: ${open.name}`} role="region">
          <h2>History: {open.name}</h2>
          {!history ? (
            <p className="small">Loading…</p>
          ) : history.length === 0 ? (
            <p className="small">No movements yet.</p>
          ) : (
            <table className="sales-table" aria-label="Movements">
              <thead>
                <tr>
                  <th scope="col">When</th>
                  <th scope="col">What</th>
                  <th scope="col" className="num">
                    Change
                  </th>
                  <th scope="col">Stock</th>
                  <th scope="col">From</th>
                  <th scope="col">By</th>
                </tr>
              </thead>
              <tbody>
                {history.map((h) => (
                  <tr key={h.movementId} data-testid="history-row">
                    <td>{h.localTime}</td>
                    <td>{MOVEMENT[h.movementType] ?? h.movementType}</td>
                    <td className="num">{h.deltaMilli.startsWith("-") ? formatMilli(h.deltaMilli) : `+${formatMilli(h.deltaMilli)}`}</td>
                    <td>{h.bucket === "damaged" ? "Damaged" : "Sellable"}</td>
                    <td className="mono">{h.sourceNumber}</td>
                    <td>{h.actorName}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>
      )}
    </section>
  );
}

function when(ms: string): string {
  const d = new Date(parseInt(ms, 10)); // a timestamp, not money
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
}
