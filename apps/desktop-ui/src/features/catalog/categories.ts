import type { CategoryDto } from "../../ipc/bindings";

/** Active categories in tree order with their depth (1 = top), for lists and pickers. */
export function flatTree(all: CategoryDto[]): { category: CategoryDto; depth: number }[] {
  const active = all.filter((c) => !c.archived);
  const out: { category: CategoryDto; depth: number }[] = [];
  const walk = (parent: string | null, depth: number) => {
    for (const c of active.filter((x) => x.parentId === parent).sort((a, b) => a.name.localeCompare(b.name))) {
      out.push({ category: c, depth });
      walk(c.categoryId, depth + 1);
    }
  };
  walk(null, 1);
  return out;
}
