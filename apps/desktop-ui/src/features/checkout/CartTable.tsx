import { useEffect, useRef, type KeyboardEvent } from "react";
import type { CartLine, CartView, Discount } from "../../ipc";
import { formatMilli, ppmToPercent } from "../../lib/decimal";
import { formatMoney } from "../../lib/money";

export type Editing = { lineId: string; field: "qty" | "discount" } | null;

/**
 * The cart (E09-T03): name / qty / unit price / line total, with quantity and discount
 * edited inline in the row. The selected row is what F3, F4, Del and numpad +/- act on.
 */
export function CartTable({
  cart,
  selected,
  highlight,
  editing,
  onSelect,
  onCommit,
  onCancel,
}: {
  cart: CartView;
  selected: string | null;
  highlight: string | null;
  editing: Editing;
  onSelect: (lineId: string) => void;
  onCommit: (lineId: string, field: "qty" | "discount", value: string) => void;
  onCancel: () => void;
}) {
  const money = (minor: string) => formatMoney(minor, { exponent: cart.currencyExponent });
  const selectedRow = useRef<HTMLTableRowElement | null>(null);
  // Braces matter: newer Chromium returns a Promise from scrollIntoView, and an effect must
  // return nothing or a cleanup function.
  useEffect(() => {
    selectedRow.current?.scrollIntoView({ block: "nearest" });
  }, [selected]);

  if (cart.lines.length === 0) {
    return (
      <div className="cart-empty">
        <p>Scan an item, or press F2 to search.</p>
      </div>
    );
  }
  return (
    <table className="cart-table" aria-label="Cart">
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
        </tr>
      </thead>
      <tbody>
        {cart.lines.map((l) => (
          <tr
            key={l.lineId}
            ref={l.lineId === selected ? selectedRow : undefined}
            aria-selected={l.lineId === selected}
            className={[l.lineId === selected ? "selected" : "", l.lineId === highlight ? "flash" : ""].join(" ")}
            onClick={() => onSelect(l.lineId)}
            data-testid="cart-line"
          >
            <td>
              <span className="item-name">{l.name}</span>
              {l.label && <span className="item-label"> {l.label}</span>}
              {cart.repricedLines.includes(l.lineId) && <span className="tag">price changed</span>}
              {l.onlyInStockMilli != null && (
                <span className="stock-hint" data-testid="stock-hint">
                  {l.onlyInStockMilli.startsWith("-") || l.onlyInStockMilli === "0"
                    ? "None in stock"
                    : `Only ${formatMilli(l.onlyInStockMilli)} in stock`}
                </span>
              )}
            </td>
            <td className="num">
              {editing?.lineId === l.lineId && editing.field === "qty" ? (
                <InlineInput label={`Quantity for ${l.name}`} initial={formatMilli(l.quantityMilli)} onCommit={(v) => onCommit(l.lineId, "qty", v)} onCancel={onCancel} />
              ) : (
                <span data-testid="qty">
                  {formatMilli(l.quantityMilli)}
                  {l.unit !== "ea" && ` ${l.unit}`}
                </span>
              )}
            </td>
            <td className="num">
              {money(l.unitPriceMinor)}
              {l.priceOverridden && <span className="tag">override</span>}
            </td>
            <td className="num">
              {editing?.lineId === l.lineId && editing.field === "discount" ? (
                <InlineInput label={`Discount % for ${l.name}`} initial={discountPercent(l)} onCommit={(v) => onCommit(l.lineId, "discount", v)} onCancel={onCancel} />
              ) : (
                <span>{l.discountMinor === "0" ? "" : `-${money(l.discountMinor)}`}{describeDiscount(l.lineDiscount)}</span>
              )}
            </td>
            <td className="num line-total">{money(l.totalMinor)}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

function discountPercent(l: CartLine): string {
  return l.lineDiscount?.kind === "percent" ? ppmToPercent(l.lineDiscount.ratePpm) : "";
}

function describeDiscount(d: Discount | null): string {
  return d?.kind === "percent" ? ` (${ppmToPercent(d.ratePpm)}%)` : "";
}

function InlineInput({
  label,
  initial,
  onCommit,
  onCancel,
}: {
  label: string;
  initial: string;
  onCommit: (v: string) => void;
  onCancel: () => void;
}) {
  const ref = useRef<HTMLInputElement | null>(null);
  useEffect(() => {
    ref.current?.focus();
    ref.current?.select();
  }, []);
  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter") {
      e.preventDefault();
      onCommit(e.currentTarget.value);
    } else if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      onCancel();
    }
  };
  return <input ref={ref} className="inline-edit" aria-label={label} inputMode="decimal" defaultValue={initial} onKeyDown={onKeyDown} onBlur={onCancel} />;
}
