import {
  ArrowLeft,
  FolderTree,
} from "../../ui/icons";
import { useCallback, useEffect, useState, type FormEvent } from "react";
import { commands, type IpcError } from "../../ipc";
import type { CategoryDto } from "../../ipc/bindings";
import { useT, type T } from "../../i18n";
import { describeError } from "../shell/errors";
import { flatTree } from "./categories";

const MAX_DEPTH = 3;

function message(e: IpcError, t: T): string {
  if (e.code === "VALIDATION_FAILED" && e.details?.field === "parent") {
    return t("That would put categories more than 3 levels deep, or a category inside itself.");
  }
  if (e.code === "VALIDATION_FAILED" && e.details?.field === "name") return t("Enter a name.");
  if (e.code === "CONFLICT_VERSION") return t("It was changed meanwhile; the current tree is shown.");
  return e.code === "VALIDATION_FAILED" ? t("Move or archive its subcategories first.") : describeError(e);
}

/**
 * Product categories (E06-T05): a tree at most 3 levels deep. Add, rename, move under
 * another category, or archive one without subcategories. Rust enforces depth and cycles.
 */
export function CategoriesScreen({ onBack }: { onBack: () => void }) {
  const t = useT();
  const [all, setAll] = useState<CategoryDto[]>([]);
  const [name, setName] = useState("");
  const [parent, setParent] = useState<string>("");
  const [editing, setEditing] = useState<{
    id: string;
    name: string;
    parent: string;
  } | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(() => {
    void commands.categoriesList().then((res) => (res.status === "ok" ? setAll(res.data) : setError(describeError(res.error))));
  }, []);
  useEffect(load, [load]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || e.defaultPrevented) return;
      e.preventDefault();
      if (editing) setEditing(null);
      else onBack();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [editing, onBack]);

  const tree = flatTree(all);
  const done = (res: { status: "ok" } | { status: "error"; error: IpcError }) => {
    if (res.status === "ok") {
      setError(null);
      setEditing(null);
    } else setError(message(res.error, t));
    load();
    return res.status === "ok";
  };

  async function add(e: FormEvent) {
    e.preventDefault();
    if (done(await commands.categorySave(null, name, parent || null, null))) setName("");
  }

  return (
    <main className="sales">
      <header className="returns-header">
        <h1 className="page-title">
          <FolderTree />
          {t("Categories")}
        </h1>
        <button type="button" className="secondary" onClick={onBack}>
          <ArrowLeft />
          {t("Back to products")} <kbd>Esc</kbd>
        </button>
      </header>
      <form className="card sales-filters" aria-label={t("Add a category")} onSubmit={(e) => void add(e)}>
        <label>
          {t("New category")}
          <input value={name} onChange={(e) => setName(e.target.value)} maxLength={80} />
        </label>
        <label>
          {t("Inside")}
          <select value={parent} onChange={(e) => setParent(e.target.value)}>
            <option value="">{t("(top level)")}</option>
            {tree
              .filter((n) => n.depth < MAX_DEPTH)
              .map((n) => (
                <option key={n.category.categoryId} value={n.category.categoryId}>
                  {"— ".repeat(n.depth - 1)}
                  {n.category.name}
                </option>
              ))}
          </select>
        </label>
        <button type="submit" disabled={!name.trim()}>
          {t("Add")}
        </button>
      </form>
      {error && (
        <p role="alert" className="field-error">
          {error}
        </p>
      )}
      <section className="card" aria-label={t("Category tree")}>
        {tree.length === 0 && (
        <div className="empty-state">
          <span className="empty-icon">
            <FolderTree />
          </span>
          <p>{t("No categories yet.")}</p>
        </div>
      )}
        <ul className="plain-list">
          {tree.map(({ category: c, depth }) => (
            <li key={c.categoryId} data-testid="category-row" style={{ paddingLeft: `${(depth - 1) * 24}px` }}>
              {editing?.id === c.categoryId ? (
                <form
                  className="setting-edit"
                  onSubmit={(e) => {
                    e.preventDefault();
                    void commands.categorySave(c.categoryId, editing.name, editing.parent || null, c.version).then(done);
                  }}
                >
                  <input aria-label={t("Name of {name}", { name: c.name })} value={editing.name} onChange={(e) => setEditing({ ...editing, name: e.target.value })} />
                  <select aria-label={t("Move {name} into", { name: c.name })} value={editing.parent} onChange={(e) => setEditing({ ...editing, parent: e.target.value })}>
                    <option value="">{t("(top level)")}</option>
                    {tree
                      .filter((n) => n.category.categoryId !== c.categoryId)
                      .map((n) => (
                        <option key={n.category.categoryId} value={n.category.categoryId}>
                          {"— ".repeat(n.depth - 1)}
                          {n.category.name}
                        </option>
                      ))}
                  </select>
                  <button type="submit">{t("Save")}</button>
                </form>
              ) : (
                <span>
                  {c.name}{" "}
                  <button
                    type="button"
                    className="link"
                    onClick={() =>
                      setEditing({
                        id: c.categoryId,
                        name: c.name,
                        parent: c.parentId ?? "",
                      })
                    }
                  >
                    {t("Edit")}
                  </button>{" "}
                  <button
                    type="button"
                    className="link"
                    aria-label={t("Archive {name}", { name: c.name })}
                    onClick={() => void commands.categoryArchive(c.categoryId, c.version).then(done)}
                  >
                    {t("Archive")}
                  </button>
                </span>
              )}
            </li>
          ))}
        </ul>
      </section>
    </main>
  );
}
