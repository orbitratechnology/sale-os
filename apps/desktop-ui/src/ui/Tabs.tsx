import { useEffect, useRef, type KeyboardEvent, type ReactNode } from "react";
import type { LucideIcon } from "lucide-react";

export interface TabItem<T> {
  value: T;
  label: ReactNode;
  icon?: LucideIcon;
}

/** Stable DOM ids for a tab and its panel, so each panel is labelled by its tab. */
export function tabIds(base: string, value: unknown): { tab: string; panel: string } {
  const key = String(value ?? "all").replace(/[^A-Za-z0-9_-]/g, "_");
  return { tab: `${base}-tab-${key}`, panel: `${base}-panel-${key}` };
}

/** Props for the element showing the selected tab's content. */
export function tabPanelProps(base: string, value: unknown) {
  const ids = tabIds(base, value);
  return { role: "tabpanel" as const, id: ids.panel, "aria-labelledby": ids.tab };
}

/**
 * The tab list whose selected tab should take focus once rendered. Selecting a tab can swap the
 * whole screen (Stock's two views are different components), remounting the list; the new
 * list then picks focus up from here so the keyboard user stays on the tabs.
 */
let focusAfterRender: string | null = null;

/**
 * A tab list following the WAI-ARIA tabs pattern: the list is one Tab stop (the selected tab);
 * arrow keys move between tabs (Left/Right, or Up/Down when vertical), Home and End jump to the
 * ends, and selection follows focus. Panels are labelled by their tab via `tabPanelProps`.
 *
 * Focus is roved in React (tabindex 0 on the selected tab, -1 on the rest) rather than with the
 * newer `focusgroup` attribute, so it works on every WebView2 runtime without a polyfill.
 */
export function TabList<T>({
  label,
  idBase,
  tabs,
  selected,
  onSelect,
  vertical,
  className,
}: {
  label: string;
  idBase: string;
  tabs: ReadonlyArray<TabItem<T>>;
  selected: T;
  onSelect: (value: T) => void;
  vertical?: boolean;
  className?: string;
}) {
  const refs = useRef<Array<HTMLButtonElement | null>>([]);
  const current = Math.max(
    0,
    tabs.findIndex((t) => t.value === selected),
  );

  useEffect(() => {
    if (focusAfterRender !== idBase) return;
    focusAfterRender = null;
    refs.current[current]?.focus();
  });

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const next = vertical ? "ArrowDown" : "ArrowRight";
    const prev = vertical ? "ArrowUp" : "ArrowLeft";
    let to: number | null = null;
    if (e.key === next) to = (current + 1) % tabs.length;
    else if (e.key === prev) to = (current - 1 + tabs.length) % tabs.length;
    else if (e.key === "Home") to = 0;
    else if (e.key === "End") to = tabs.length - 1;
    if (to === null) return;
    e.preventDefault();
    const tab = tabs[to];
    if (!tab) return;
    focusAfterRender = idBase;
    onSelect(tab.value);
    refs.current[to]?.focus();
  };

  return (
    <div className={className} role="tablist" aria-label={label} aria-orientation={vertical ? "vertical" : undefined} onKeyDown={onKeyDown}>
      {tabs.map((t, i) => {
        const ids = tabIds(idBase, t.value);
        const isSelected = i === current;
        const Icon = t.icon;
        return (
          <button
            key={ids.tab}
            ref={(el) => {
              refs.current[i] = el;
            }}
            id={ids.tab}
            type="button"
            role="tab"
            aria-selected={isSelected}
            tabIndex={isSelected ? 0 : -1}
            onClick={() => onSelect(t.value)}
          >
            {Icon && <Icon />}
            {t.label}
          </button>
        );
      })}
    </div>
  );
}
