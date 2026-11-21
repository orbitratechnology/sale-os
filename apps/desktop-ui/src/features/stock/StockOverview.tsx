import {
  Inbox,
} from "../../ui/icons";
import { useEffect, useState } from "react";
import { recordOf, type RecordRef } from "../records/RecordView";
import { commands, type IpcError } from "../../ipc";
import type { StockHistoryDto, StockReportDto } from "../../ipc/bindings";
import { formatMilli } from "../../lib/decimal";
import { describeError } from "../shell/errors";
import { useT, type T } from "../../i18n";

/** Movement types in plain words; English, translated at render with `t`. */
const MOVEMENT_COPY: Record<string, string> = {
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

/** A movement type in words; an unknown type as it is. */
function movementText(type: string, t: T): string {
  const english = Object.hasOwn(MOVEMENT_COPY, type) ? MOVEMENT_COPY[type] : undefined;
  return english ? t(english) : type;
}

/**
 * Stock on hand (E11-T05): every stock-tracked item with sellable and damaged quantities, its
 * low-stock level and status, and its movement history. Figures come from the recorded
 * movements, which the banner says plainly (PRD §6.6).
 */
export function StockOverview({
  state,
  onState,
  onRecord,
}: {
  /** Kept by the stock screen, so coming back from a record finds the same item open. */
  state: OverviewState;
  onState: (s: OverviewState) => void;
  /** A history row's sale, return or stock document. */
  onRecord: (record: RecordRef) => void;
}) {
  const t = useT();
  const { lowOnly, open } = state;
  const setLowOnly = (v: boolean) => onState({ ...state, lowOnly: v });
  const setOpen = (v: OverviewState["open"]) => onState({ ...state, open: v, focus: null });
  const [report, setReport] = useState<StockReportDto | null>(null);
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

  // Back from a record: focus the row's link that opened it.
  const focus = state.focus;
  useEffect(() => {
    if (!history || !focus) return;
    document.querySelector<HTMLElement>(`[data-source="${CSS.escape(focus)}"]`)?.focus();
  }, [history, focus]);

  return (
    <section className="card return-form" aria-label={t("Stock on hand")}>
      <p className="drawer-needed" role="note">
        {t("Stock on hand is calculated from recorded movements; it is not a physical count.")}
      </p>
      <label className="check">
        <input type="checkbox" checked={lowOnly} onChange={(e) => setLowOnly(e.target.checked)} /> {t("Low and negative only")}
      </label>
      {error && (
        <p role="alert" className="field-error">
          {describeError(error)}
        </p>
      )}
      <table className="sales-table" aria-label={t("On hand")}>
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
            <th scope="col">{t("Last movement")}</th>
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
                  <span className={`stock-status ${r.status}`}>{r.status === "ok" ? t("OK") : r.status === "low" ? t("Low") : t("Negative")}</span>
                </td>
                <td className="small">{r.lastMovementMs ? when(r.lastMovementMs) : t("never")}</td>
              </tr>
            );
          })}
        </tbody>
      </table>
      {report && report.rows.length === 0 && (
        <div className="empty-state">
          <span className="empty-icon">
            <Inbox />
          </span>
          <p>{t("Nothing to show.")}</p>
        </div>
      )}

      {open && (
        <div aria-label={t("History: {name}", { name: open.name })} role="region">
          <h2>{t("History: {name}", { name: open.name })}</h2>
          {!history ? (
            <p className="small">{t("Loading…")}</p>
          ) : history.length === 0 ? (
            <p className="small">{t("No movements yet.")}</p>
          ) : (
            <table className="sales-table" aria-label={t("Movements")}>
              <thead>
                <tr>
                  <th scope="col">{t("When")}</th>
                  <th scope="col">{t("What")}</th>
                  <th scope="col" className="num">
                    {t("stock quantity::Change")}
                  </th>
                  <th scope="col">{t("Stock")}</th>
                  <th scope="col">{t("From")}</th>
                  <th scope="col">{t("By")}</th>
                </tr>
              </thead>
              <tbody>
                {history.map((h) => (
                  <tr key={h.movementId} data-testid="history-row">
                    <td>{h.localTime}</td>
                    <td>{movementText(h.movementType, t)}</td>
                    <td className="num">{h.deltaMilli.startsWith("-") ? formatMilli(h.deltaMilli) : `+${formatMilli(h.deltaMilli)}`}</td>
                    <td>{h.bucket === "damaged" ? t("Damaged") : t("Sellable")}</td>
                    <td className="mono">
                      <SourceLink
                        history={h}
                        onRecord={(r) => {
                          onState({ ...state, focus: h.sourceId });
                          onRecord(r);
                        }}
                      />
                    </td>
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

export interface OverviewState {
  lowOnly: boolean;
  open: { variantId: string; name: string } | null;
  /** The source id whose link to focus once the history has loaded. */
  focus: string | null;
}

export const INITIAL_OVERVIEW: OverviewState = { lowOnly: false, open: null, focus: null };

function SourceLink({ history: h, onRecord }: { history: StockHistoryDto; onRecord: (r: RecordRef) => void }) {
  const record = recordOf(h.sourceType, h.sourceId);
  if (!record || !h.sourceNumber) return <>{h.sourceNumber}</>;
  return (
    <button type="button" className="link mono" data-source={h.sourceId} onClick={() => onRecord(record)}>
      {h.sourceNumber}
    </button>
  );
}

function when(ms: string): string {
  const d = new Date(parseInt(ms, 10)); // a timestamp, not money
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
}
