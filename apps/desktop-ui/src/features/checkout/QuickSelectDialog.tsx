import { TabList } from "../../ui/Tabs";
import { Modal } from "../../ui/Modal";
import { useCallback, useEffect, useState } from "react";
import { commands, type IpcError } from "../../ipc";
import type { QuickProductDto, QuickSelectPageDto } from "../../ipc/bindings";
import { formatMoney } from "../../lib/money";
import { describeError } from "../shell/errors";
import { useT } from "../../i18n";

/** Tiles per page: 4 rows of 6 fit beside the cart at 1366×768. */
export const QUICK_PAGE = 24;

/**
 * The quick-select grid (E09-T04, POS-002): products flagged "quick select", by name, with
 * their categories as tabs, a page at a time. Tiles are large enough to touch (at least
 * 44 px). A product with variants asks which one. Each tap adds one to the cart and the grid
 * stays open for the next; `Esc` or Done closes it.
 */
export function QuickSelectDialog({
  exponent,
  onPick,
  onClose,
}: {
  exponent: number;
  /** `unit`: items sold by weight (`kg`, `g`) may be weighed instead of added. */
  onPick: (variantId: string, unit: string, name: string) => Promise<boolean>;
  onClose: () => void;
}) {
  const t = useT();
  const [tab, setTab] = useState<string | null>(null);
  const [offset, setOffset] = useState(0);
  const [page, setPage] = useState<QuickSelectPageDto | null>(null);
  const [error, setError] = useState<IpcError | null>(null);
  const [choosing, setChoosing] = useState<QuickProductDto | null>(null);
  const [added, setAdded] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    void commands.catalogQuickSelect(tab, offset, QUICK_PAGE).then((res) => {
      if (!live) return;
      if (res.status === "ok") {
        setPage(res.data);
        setError(null);
      } else setError(res.error);
    });
    return () => {
      live = false;
    };
  }, [tab, offset]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      e.stopPropagation();
      if (choosing) setChoosing(null);
      else onClose();
    };
    window.addEventListener("keydown", onKey, { capture: true });
    return () => window.removeEventListener("keydown", onKey, { capture: true });
  }, [choosing, onClose]);

  const pick = useCallback(
    async (name: string, variantId: string, unit: string) => {
      if (await onPick(variantId, unit, name)) setAdded(name);
      setChoosing(null);
    },
    [onPick],
  );

  const money = (minor: string) => formatMoney(minor, { exponent });
  const priceOf = (p: QuickProductDto) => {
    const prices = p.variants.map((v) => BigInt(v.priceMinor));
    const low = prices.reduce((a, b) => (b < a ? b : a));
    const high = prices.reduce((a, b) => (b > a ? b : a));
    return low === high ? money(low.toString()) : `${money(low.toString())}–${money(high.toString())}`;
  };
  const tabs = [{ categoryId: null as string | null, name: t("All") }, ...(page?.categories ?? [])];
  const total = page?.total ?? 0;

  return (
    <Modal label={t("Quick items")} className="wide quick-select" focusSelf>
        <h2>{t("Quick items")}</h2>
        <TabList
          label={t("Categories")}
          idBase="quick"
          className="receipt-widths"
          tabs={tabs.map((tab) => ({ value: tab.categoryId, label: tab.name }))}
          selected={tab}
          onSelect={(value) => {
            setTab(value);
            setOffset(0);
            setChoosing(null);
          }}
        />
        {error && (
          <p role="alert" className="field-error">
            {describeError(error)}
          </p>
        )}
        {page === null && !error && <p aria-busy="true">{t("Loading…")}</p>}
        {page && page.products.length === 0 && (
          <p>{t("No quick items yet. In Products (F6), tick “Show on the quick-select grid” for the items you sell most.")}</p>
        )}
        {choosing ? (
          <div role="group" aria-label={t("Choose a {name}", { name: choosing.name })} className="quick-grid">
            {choosing.variants.map((v) => (
              <button key={v.variantId} type="button" className="quick-tile" onClick={() => void pick(`${choosing.name} ${v.label}`, v.variantId, v.unit)}>
                <span className="quick-name">{v.label || choosing.name}</span>
                <span className="quick-price">{money(v.priceMinor)}</span>
              </button>
            ))}
            <button type="button" className="quick-tile secondary" onClick={() => setChoosing(null)}>
              {t("Back")}
            </button>
          </div>
        ) : (
          page &&
          page.products.length > 0 && (
            <div role="group" aria-label={t("Items")} className="quick-grid">
              {page.products.map((p) => {
                const only = p.variants.length === 1 ? p.variants[0] : undefined;
                return (
                  <button
                    key={p.productId}
                    type="button"
                    className="quick-tile"
                    data-testid="quick-tile"
                    aria-haspopup={only ? undefined : "true"}
                    onClick={() => (only ? void pick(p.name, only.variantId, only.unit) : setChoosing(p))}
                  >
                    <span className="quick-name">{p.name}</span>
                    <span className="quick-price">{priceOf(p)}</span>
                    {!only && <span className="small">{t("{n} options", { n: p.variants.length })}</span>}
                  </button>
                );
              })}
            </div>
          )
        )}
        <p role="status" className="small">
          {added ? t("Added {name}.", { name: added }) : ""}
        </p>
        <div className="actions">
          {total > QUICK_PAGE && (
            <>
              <button type="button" className="secondary" disabled={offset === 0} onClick={() => setOffset(Math.max(0, offset - QUICK_PAGE))}>
                {t("Previous")}
              </button>
              <span className="small">
                {t("Page {n} of {of}", { n: Math.floor(offset / QUICK_PAGE) + 1, of: Math.ceil(total / QUICK_PAGE) })}
              </span>
              <button type="button" className="secondary" disabled={offset + QUICK_PAGE >= total} onClick={() => setOffset(offset + QUICK_PAGE)}>
                {t("Next")}
              </button>
            </>
          )}
          <button type="button" onClick={onClose}>
            {t("Done")}
          </button>
        </div>
    </Modal>
  );
}
