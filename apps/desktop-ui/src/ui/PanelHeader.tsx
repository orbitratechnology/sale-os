import type { ReactNode } from "react";
import type { LucideIcon } from "lucide-react";

/**
 * The heading of a settings page: an icon in a soft tile, the page's name and one plain
 * sentence on what it is for. Every settings page starts with one, so they read alike.
 */
export function PanelHeader({ icon: Icon, title, children }: { icon: LucideIcon; title: string; children?: ReactNode }) {
  return (
    <header className="panel-header">
      <span className="icon-tile" aria-hidden="true">
        <Icon />
      </span>
      <div>
        <h2>{title}</h2>
        {children && <p className="small">{children}</p>}
      </div>
    </header>
  );
}

/** A topic inside a settings page: a small icon beside its heading. */
export function SectionTitle({ icon: Icon, children, level = 3, id }: { icon: LucideIcon; children: ReactNode; level?: 2 | 3; id?: string }) {
  const H = level === 2 ? "h2" : "h3";
  return (
    <H className="section-title" id={id}>
      <Icon />
      {children}
    </H>
  );
}
