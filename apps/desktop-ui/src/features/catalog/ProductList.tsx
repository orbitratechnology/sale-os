import {
  ArrowLeft,
  FolderTree,
  Package,
  PackageSearch,
  Plus,
  Upload,
} from "../../ui/icons";
import { useCallback, useEffect, useRef, useState, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { commands, type IpcError, type Workspace } from "../../ipc";
import type { CategoryDto, ProductRowDto_Serialize as ProductRowDto, ProductStatusDto } from "../../ipc/bindings";
import { formatMilli } from "../../lib/decimal";
import { formatMoney } from "../../lib/money";
import { useT } from "../../i18n";
import { describeError } from "../shell/errors";
import { flatTree } from "./categories";

/** Fixed row height: the list positions rows by index, so 50k rows cost the same as 50. */
export const ROW_PX = 44;
const PAGE = 200;
const OVERSCAN = 8;
export const FILTER_DEBOUNCE_MS = 150;

interface Filter {
  query: string;
  status: ProductStatusDto;
  /** "low": tracked products that need restocking (low or negative stock). */
  tracked: "any" | "yes" | "no" | "low";
  /** A category id, or "" for any. */
  category: string;
}

/**
 * The product list (E06-T06a): virtualized, filtered in Rust, pages fetched as you scroll.
 * Columns: name, SKU, price, variants, barcodes, stock tracking.
 */
export function ProductList({
  workspace,
  canEdit,
  onOpen,
  onNew,
  onBack,
  onCategories,
  onImport,
}: {
  workspace: Workspace;
  canEdit: boolean;
  onOpen: (productId: string) => void;
  onNew: () => void;
  onBack: () => void;
  /** Editors only: open the category tree. */
  onCategories?: () => void;
  /** Owners and managers: import products from a CSV file. */
  onImport?: () => void;
}) {
  const t = useT();
  const [filter, setFilter] = useState<Filter>({ query: "", status: "active", tracked: "any", category: "" });
  const [categories, setCategories] = useState<CategoryDto[]>([]);
  useEffect(() => {
    void commands.categoriesList().then((res) => res.status === "ok" && setCategories(res.data));
  }, []);
  const [applied, setApplied] = useState<Filter>(filter);
  const [total, setTotal] = useState<number | null>(null);
  const [rows, setRows] = useState<Map<number, ProductRowDto>>(new Map());
  const [error, setError] = useState<IpcError | null>(null);
  const [scrollTop, setScrollTop] = useState(0);
  const [height, setHeight] = useState(600);
  const viewport = useRef<HTMLDivElement | null>(null);
  const generation = useRef(0);
  const requested = useRef(new Set<number>());
  const money = (minor: string) => formatMoney(minor, { exponent: workspace.currencyExponent });

  const loadPage = useCallback((page: number, gen: number, f: Filter) => {
    if (requested.current.has(page)) return;
    requested.current.add(page);
    void commands
      .productList(
        {
          query: f.query,
          status: f.status,
          trackStock: f.tracked === "yes" ? true : f.tracked === "no" ? false : null,
          categoryId: f.category || null,
          lowStock: f.tracked === "low",
        },
        page * PAGE,
        PAGE,
      )
      .then((res) => {
        if (gen !== generation.current) return; // an older filter's answer
        if (res.status === "error") return setError(res.error);
        setTotal(res.data.total);
        setRows((prev) => {
          const next = new Map(prev);
          res.data.rows.forEach((r, i) => next.set(page * PAGE + i, r));
          return next;
        });
      });
  }, []);

  /** A new filter starts over from the top. */
  const start = useCallback(
    (f: Filter) => {
      const gen = ++generation.current;
      requested.current = new Set();
      setApplied(f);
      setRows(new Map());
      setTotal(null);
      setError(null);
      setScrollTop(0);
      viewport.current?.scrollTo({ top: 0 });
      loadPage(0, gen, f);
    },
    [loadPage],
  );

  // Typing is debounced; the selects apply at once. The same filter never reloads.
  const lastKey = useRef("");
  const lastQuery = useRef("");
  useEffect(() => {
    const key = JSON.stringify(filter);
    const delay = filter.query === lastQuery.current ? 0 : FILTER_DEBOUNCE_MS;
    const timer = setTimeout(() => {
      if (key === lastKey.current) return;
      lastKey.current = key;
      lastQuery.current = filter.query;
      start(filter);
    }, delay);
    return () => clearTimeout(timer);
  }, [filter, start]);

  useEffect(() => {
    const el = viewport.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setHeight(el.clientHeight));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  const count = total ?? 0;
  const first = Math.max(0, Math.floor(scrollTop / ROW_PX) - OVERSCAN);
  const last = Math.min(count, Math.ceil((scrollTop + height) / ROW_PX) + OVERSCAN);

  // Fetch the pages the visible window needs.
  useEffect(() => {
    if (total === null) return;
    for (let p = Math.floor(first / PAGE); p <= Math.floor(Math.max(first, last - 1) / PAGE); p++) {
      loadPage(p, generation.current, applied);
    }
  }, [first, last, total, applied, loadPage]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !e.defaultPrevented) onBack();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onBack]);

  /**
   * Keyboard movement through the whole list, not just the rows rendered: scroll the target row
   * into view, then focus its button once it has rendered (and its page has loaded).
   */
  const focusRow = (i: number) => {
    const vp = viewport.current;
    if (!vp || count === 0) return;
    const row = Math.min(Math.max(i, 0), count - 1);
    const top = row * ROW_PX;
    if (top < vp.scrollTop) vp.scrollTop = top;
    else if (top + ROW_PX > vp.scrollTop + vp.clientHeight) vp.scrollTop = top + ROW_PX - vp.clientHeight;
    let tries = 30;
    const attempt = () => {
      const button = vp.querySelector<HTMLButtonElement>(`[data-row="${row}"] .plist-open`);
      if (button) button.focus();
      else if (tries-- > 0) requestAnimationFrame(attempt);
    };
    requestAnimationFrame(attempt);
  };
  const onListKey = (e: ReactKeyboardEvent<HTMLDivElement>) => {
    const at = (e.target as HTMLElement).closest<HTMLElement>("[data-row]")?.dataset.row;
    if (at === undefined) return;
    const i = parseInt(at, 10); // a row index, not money
    const page = Math.max(1, Math.floor((viewport.current?.clientHeight ?? ROW_PX) / ROW_PX) - 1);
    const to =
      e.key === "ArrowDown" ? i + 1 : e.key === "ArrowUp" ? i - 1 : e.key === "PageDown" ? i + page : e.key === "PageUp" ? i - page : e.key === "Home" ? 0 : e.key === "End" ? count - 1 : null;
    if (to === null) return;
    e.preventDefault();
    focusRow(to);
  };

  const visible = [];
  for (let i = first; i < last; i++) {
    const r = rows.get(i);
    visible.push(
      <div key={i} role="row" aria-rowindex={i + 2} data-row={i} className="plist-row plist-cols" style={{ transform: `translateY(${i * ROW_PX}px)` }}>
        {r ? (
          <>
            {/* Cells are the row's own children (table semantics); the name opens the product, and
                its button stretches over the whole row so a click anywhere on it opens it too. */}
            <span role="cell" className="item-name">
              <button type="button" className="plist-open" onClick={() => onOpen(r.productId)} data-testid="product-row">
                {r.name}
              </button>
              {r.archived && <span className="tag">{t("archived")}</span>}
            </span>
            <span role="cell" className="mono">
              {r.sku ?? ""}
            </span>
            <span role="cell" className="num">
              {r.priceMinMinor === r.priceMaxMinor ? money(r.priceMinMinor) : `${money(r.priceMinMinor)}–${money(r.priceMaxMinor)}`}
            </span>
            <span role="cell" className="num">
              {r.variantCount}
            </span>
            <span role="cell" className="num">
              {r.barcodeCount}
            </span>
            <span role="cell">
              {r.onHandMilli == null ? (
                t("Not tracked")
              ) : (
                <>
                  {formatMilli(r.onHandMilli)}
                  {r.negativeStock && <span className="stock-flag"> {t("Negative stock")}</span>}
                  {r.lowStock && <span className="stock-flag low"> {t("Low stock")}</span>}
                  {r.syncConflict && (
                    <span className="stock-flag" title={t("Changed here and in the cloud: this till uses its own values until resolved")}>
                      {" "}
                      {t("Sync conflict")}
                    </span>
                  )}
                </>
              )}
            </span>
          </>
        ) : (
          <span role="cell" className="small">
            {t("Loading…")}
          </span>
        )}
      </div>,
    );
  }

  return (
    <main className="products">
      <header className="products-head">
        <h1 className="page-title">
          <Package />
          {t("Products")}
        </h1>
        <span className="small" aria-live="polite" data-testid="product-total">
          {total === null ? "" : total === 1 ? t("{n} product", { n: total }) : t("{n} products", { n: total })}
        </span>
        {canEdit && onImport && (
          <button type="button" className="secondary" onClick={onImport}>
            <Upload />
            {t("Import CSV")}
          </button>
        )}
        {canEdit && onCategories && (
          <button type="button" className="secondary" onClick={onCategories}>
            <FolderTree />
            {t("Categories")}
          </button>
        )}
        {canEdit && (
          <button type="button" onClick={onNew}>
            <Plus />
            {t("New product")}
          </button>
        )}
        <button type="button" className="secondary" onClick={onBack}>
          <ArrowLeft />
          {t("Back to till")} <kbd>Esc</kbd>
        </button>
      </header>
      <div className="products-filters">
        <input
          type="search"
          aria-label={t("Filter by name, SKU or barcode")}
          placeholder={t("Filter by name, SKU or barcode")}
          value={filter.query}
          autoFocus
          onChange={(e) => setFilter((f) => ({ ...f, query: e.target.value }))}
        />
        <label>
          {t("Status")}
          <select value={filter.status} onChange={(e) => setFilter((f) => ({ ...f, status: e.target.value as ProductStatusDto }))}>
            <option value="active">{t("Active")}</option>
            <option value="archived">{t("Archived")}</option>
            <option value="all">{t("All")}</option>
          </select>
        </label>
        <label>
          {t("Stock")}
          <select value={filter.tracked} onChange={(e) => setFilter((f) => ({ ...f, tracked: e.target.value as Filter["tracked"] }))}>
            <option value="any">{t("Any")}</option>
            <option value="yes">{t("Tracked")}</option>
            <option value="no">{t("Not tracked")}</option>
            <option value="low">{t("Low stock")}</option>
          </select>
        </label>
        {categories.length > 0 && (
          <label>
            {t("Category")}
            <select value={filter.category} onChange={(e) => setFilter((f) => ({ ...f, category: e.target.value }))}>
              <option value="">{t("Any")}</option>
              {flatTree(categories).map(({ category: c, depth }) => (
                <option key={c.categoryId} value={c.categoryId}>
                  {"— ".repeat(depth - 1)}
                  {c.name}
                </option>
              ))}
            </select>
          </label>
        )}
      </div>
      {error && (
        <p role="alert" className="field-error">
          {describeError(error)}
        </p>
      )}
      <div role="table" aria-label={t("Products")} aria-rowcount={count + 1} className="plist">
        <div role="row" aria-rowindex={1} className="plist-head plist-cols">
          <span role="columnheader">{t("Name")}</span>
          <span role="columnheader">SKU</span>
          <span role="columnheader" className="num">
            {t("Price")}
          </span>
          <span role="columnheader" className="num">
            {t("Variants")}
          </span>
          <span role="columnheader" className="num">
            {t("Barcodes")}
          </span>
          <span role="columnheader">{t("Stock")}</span>
        </div>
        <div ref={viewport} className="plist-viewport" onScroll={(e) => setScrollTop(e.currentTarget.scrollTop)} onKeyDown={onListKey} data-testid="product-viewport">
          <div style={{ height: count * ROW_PX, position: "relative" }}>{visible}</div>
          {total === 0 && (
        <div className="empty-state">
          <span className="empty-icon">
            <PackageSearch />
          </span>
          <p>{t("No products match.")}</p>
        </div>
      )}
        </div>
      </div>
    </main>
  );
}
