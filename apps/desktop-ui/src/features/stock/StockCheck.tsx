import { CircleCheck, RefreshCw, TriangleExclamationPoint } from "../../ui/icons";
import { useEffect, useState } from "react";
import { commands, type IpcError } from "../../ipc";
import type { StockDriftDto_Serialize as StockDrift } from "../../ipc/bindings";
import { formatMilli } from "../../lib/decimal";
import { describeError } from "../shell/errors";
import { useT } from "../../i18n";
import { Dialog } from "../checkout/Dialog";

/**
 * Check stock figures (E11-T06): each item's stock on hand is a running total kept beside the
 * movements it comes from. The check adds the movements up again and lists every figure that
 * differs. The owner can rebuild the figures from the movements (Rust does it in one
 * transaction and audits it); the screen then checks again and shows the result.
 */
export function StockCheck({ canRebuild }: { canRebuild: boolean }) {
  const t = useT();
  const [drift, setDrift] = useState<StockDrift[] | null>(null);
  const [rebuilt, setRebuilt] = useState<number | null>(null);
  // The check runs when the view opens.
  const [busy, setBusy] = useState(true);
  const [confirming, setConfirming] = useState(false);
  const [error, setError] = useState<IpcError | null>(null);

  const shown = (res: Awaited<ReturnType<typeof commands.stockVerify>>) => {
    setBusy(false);
    if (res.status === "ok") setDrift(res.data);
    else setError(res.error);
  };

  useEffect(() => {
    let live = true;
    void commands.stockVerify().then((res) => {
      if (live) shown(res);
    });
    return () => {
      live = false;
    };
  }, []);

  async function verify() {
    setBusy(true);
    setError(null);
    shown(await commands.stockVerify());
  }

  async function rebuild() {
    setConfirming(false);
    setBusy(true);
    setError(null);
    const res = await commands.stockRebuild();
    if (res.status !== "ok") {
      setBusy(false);
      return setError(res.error);
    }
    setRebuilt(res.data.length);
    // Check again, so what is shown is what the figures say now.
    await verify();
  }

  const n = drift?.length ?? 0;
  return (
    <section className="card return-form" aria-label={t("Check stock figures")}>
      <h2>{t("Check stock figures")}</h2>
      <p className="small">
        {t("Stock on hand is kept as a running total for each item. This check adds up each item's recorded movements again and compares the two.")}
      </p>
      <div aria-live="polite" className="stack">
        {rebuilt !== null && (
          <p role="status" data-testid="stock-rebuilt">
            {rebuilt === 1
              ? t("1 stock figure was rebuilt from its history.")
              : t("{n} stock figures were rebuilt from their history.", { n: rebuilt })}
          </p>
        )}
        {busy ? (
          <p className="small">{t("Checking…")}</p>
        ) : error ? (
          <p role="alert" className="field-error">
            {describeError(error)}
          </p>
        ) : drift === null ? null : n === 0 ? (
          <p className="callout-ok" data-testid="stock-check-result">
            <CircleCheck />
            <span>{t("All stock figures match their history.")}</span>
          </p>
        ) : (
          <p className="callout-warning" data-testid="stock-check-result">
            <TriangleExclamationPoint />
            <span>
              {n === 1
                ? t("1 stock figure does not match its history.")
                : t("{n} stock figures do not match their history.", { n })}{" "}
              {canRebuild
                ? t("Rebuilding sets each one to the total of its recorded movements.")
                : t("Only the owner can rebuild them: ask the owner to open this check.")}
            </span>
          </p>
        )}
      </div>
      {drift && n > 0 && !busy && (
        <table className="sales-table" aria-label={t("Stock figures that do not match")}>
          <thead>
            <tr>
              <th scope="col">{t("Item")}</th>
              <th scope="col">{t("Location")}</th>
              <th scope="col">{t("Stock")}</th>
              <th scope="col" className="num">
                {t("Recorded")}
              </th>
              <th scope="col" className="num">
                {t("From history")}
              </th>
            </tr>
          </thead>
          <tbody>
            {drift.map((d) => (
              <tr key={`${d.locationId}:${d.variantId}:${d.bucket}`} data-testid="drift-row">
                <th scope="row">
                  {d.itemName || t("Unknown item")}
                  {d.sku && <span className="small"> · {d.sku}</span>}
                </th>
                <td>{d.locationName || t("Unknown location")}</td>
                <td>{d.bucket === "damaged" ? t("Damaged") : t("Sellable")}</td>
                <td className="num">{formatMilli(d.recordedMilli)}</td>
                <td className="num">{formatMilli(d.fromMovementsMilli)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      <div className="actions">
        <button type="button" className="secondary" disabled={busy} onClick={() => void verify()}>
          <RefreshCw />
          {t("Check again")}
        </button>
        {canRebuild && n > 0 && (
          <button type="button" className="danger" disabled={busy} onClick={() => setConfirming(true)}>
            {t("Rebuild stock figures")}
          </button>
        )}
      </div>
      {confirming && (
        <Dialog
          title={t("Rebuild stock figures?")}
          alert
          safeFocus
          danger
          confirmLabel={t("Rebuild")}
          onConfirm={() => void rebuild()}
          onCancel={() => setConfirming(false)}
        >
          <p>
            {n === 1
              ? t("1 stock figure will be set to the total of its recorded movements.")
              : t("{n} stock figures will be set to the totals of their recorded movements.", { n })}
          </p>
          <p>{t("Sales, returns and stock documents are not changed. The rebuild is recorded in the audit log.")}</p>
        </Dialog>
      )}
    </section>
  );
}
