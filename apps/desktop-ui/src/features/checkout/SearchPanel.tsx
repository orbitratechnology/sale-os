import { forwardRef, useEffect, useId, useImperativeHandle, useRef, useState, type KeyboardEvent } from "react";
import { commands, type CatalogHit } from "../../ipc";
import { formatMoney } from "../../lib/money";

/** Search as you type (E09-T04). */
export const SEARCH_DEBOUNCE_MS = 80;
const LIMIT = 8;

export interface SearchHandle {
  focus: (text?: string) => void;
}

/**
 * Name / SKU / barcode search: ↑/↓ choose, Enter adds, Esc closes. Results that arrive
 * after a newer query are dropped.
 */
export const SearchPanel = forwardRef<SearchHandle, { exponent: number; onPick: (hit: CatalogHit) => void }>(
  function SearchPanel({ exponent, onPick }, handle) {
    const [query, setQuery] = useState("");
    const [hits, setHits] = useState<CatalogHit[]>([]);
    const [active, setActive] = useState(0);
    const input = useRef<HTMLInputElement | null>(null);
    const latest = useRef(0);
    const listId = useId();

    useImperativeHandle(handle, () => ({
      focus: (text) => {
        if (text !== undefined) setQuery(text);
        input.current?.focus();
      },
    }));

    useEffect(() => {
      const q = query.trim();
      const ticket = ++latest.current;
      if (!q) {
        setHits([]);
        return;
      }
      const t = setTimeout(() => {
        void commands.catalogSearch(q, LIMIT).then((res) => {
          if (ticket !== latest.current) return;
          setHits(res.status === "ok" ? res.data : []);
          setActive(0);
        });
      }, SEARCH_DEBOUNCE_MS);
      return () => clearTimeout(t);
    }, [query]);

    const close = () => {
      setQuery("");
      setHits([]);
      input.current?.blur();
    };

    const pick = (hit: CatalogHit | undefined) => {
      if (!hit) return;
      onPick(hit);
      close();
    };

    const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setActive((a) => Math.min(a + 1, hits.length - 1));
      } else if (e.key === "ArrowUp") {
        e.preventDefault();
        setActive((a) => Math.max(a - 1, 0));
      } else if (e.key === "Enter") {
        e.preventDefault();
        pick(hits[active]);
      } else if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        close();
      }
    };

    const open = hits.length > 0;
    return (
      <div className="search">
        <input
          ref={input}
          type="search"
          role="combobox"
          aria-label="Search products (F2)"
          aria-expanded={open}
          aria-controls={listId}
          aria-activedescendant={open ? `${listId}-${active}` : undefined}
          placeholder="Search products (F2)"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={onKeyDown}
          autoComplete="off"
        />
        {open && (
          <ul id={listId} role="listbox" className="search-results" aria-label="Search results">
            {hits.map((h, i) => (
              <li
                key={h.variantId}
                id={`${listId}-${i}`}
                role="option"
                aria-selected={i === active}
                className={i === active ? "active" : undefined}
                onMouseDown={(e) => {
                  e.preventDefault();
                  pick(h);
                }}
              >
                <span>
                  {h.name}
                  {h.label && <span className="item-label"> {h.label}</span>}
                  {h.sku && <span className="small mono"> {h.sku}</span>}
                </span>
                <span className="num">{formatMoney(h.priceMinor, { exponent })}</span>
              </li>
            ))}
          </ul>
        )}
        {query.trim() && !open && <p className="search-empty small">No matches yet…</p>}
      </div>
    );
  },
);
