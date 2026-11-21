import { useCallback, useEffect, useState, type FormEvent } from "react";
import { commands, type IpcError } from "../../ipc";
import type { Role, StaffDto } from "../../ipc/bindings";
import { describeError } from "../shell/errors";
import { useT, type T } from "../../i18n";
import { SectionTitle } from "../../ui/PanelHeader";
import { UserCheck, UserPlus, UserX, Users } from "../../ui/icons";

/** Each role's name and what it may do, in the order the role pickers list them. */
const ROLE_COPY: Record<Role, { label: string; help: string }> = {
  cashier: { label: "Cashier", help: "Cashiers can sell, make returns with a manager's approval, and count the drawer (without seeing the expected amount)." },
  manager: { label: "Manager", help: "Managers can do everything a cashier can, approve, edit products and stock, and see reports." },
  owner: { label: "Owner", help: "Owners can do everything, including staff, settings and the cloud." },
};
const ROLES: Role[] = ["cashier", "manager", "owner"];
const roleLabel = (r: Role, t: T) => (ROLE_COPY[r] ? t(ROLE_COPY[r].label) : r);

type Field = "displayName" | "pin" | "role";

/** The message for a staff error, and the input it belongs next to. */
function problemOf(e: IpcError, t: T): { field: Field | null; text: string } {
  const field = (e.details?.field ?? null) as Field | null;
  const value = e.details?.value ?? null;
  if (e.code === "CONFLICT_VERSION") return { field: null, text: t("Someone changed this staff member meanwhile; the list is updated.") };
  if (e.code === "VALIDATION_FAILED") {
    if (field === "displayName" && value === "taken") return { field, text: t("Another active staff member already has this name.") };
    if (field === "displayName" && value) return { field, text: t("Names can be at most {n} characters.", { n: value }) };
    if (field === "displayName") return { field, text: t("Enter a name.") };
    if (field === "pin" && value === "same") return { field, text: t("The new PIN must differ from the current one.") };
    if (field === "pin") return { field, text: t("A PIN is 4 to 8 digits.") };
    if (field === "role") return { field, text: t("The business must keep at least one active owner.") };
  }
  return { field: null, text: describeError(e) };
}

/** Checked here before sending, so typos get an answer at once; Rust checks again. */
function pinProblem(pin: string, again: string, t: T): string | null {
  if (!/^\d{4,8}$/.test(pin)) return t("A PIN is 4 to 8 digits.");
  if (pin !== again) return t("The two PINs differ.");
  return null;
}

type Editing =
  | { kind: "none" }
  | { kind: "edit"; user: StaffDto; name: string; role: Role }
  | { kind: "pin"; user: StaffDto; pin: string; again: string }
  | { kind: "deactivate"; user: StaffDto }
  | { kind: "reactivate"; user: StaffDto };

/**
 * Staff (E05-T06, owner): who can sign in to this till and what they may do. Each person
 * has their own PIN; nobody shares the owner's. An owner can rename, change a role, set a
 * temporary PIN (changed at the next sign-in) and deactivate. Deactivated staff keep their
 * history and can no longer sign in; an owner can reactivate them (same role and PIN). Rust
 * refuses to leave the business without an owner.
 */
export function StaffPanel() {
  const t = useT();
  const [staff, setStaff] = useState<StaffDto[] | null>(null);
  const [problem, setProblem] = useState<{ field: Field | null; text: string } | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [editing, setEditing] = useState<Editing>({ kind: "none" });
  const [draft, setDraft] = useState({ name: "", role: "cashier" as Role, pin: "", again: "" });
  const [newProblem, setNewProblem] = useState<{ field: Field | null; text: string } | null>(null);

  const load = useCallback(() => {
    void commands.staffList().then((res) => {
      if (res.status !== "ok") return setProblem({ field: null, text: describeError(res.error) });
      setStaff(res.data);
    });
  }, []);
  useEffect(load, [load]);

  if (!staff) return problem ? <p role="alert" className="field-error">{problem.text}</p> : <p className="small">{t("Loading…")}</p>;

  /** Applies a command's result: the updated row, or the problem (reloading on a conflict). */
  function settle(res: { status: "ok"; data: StaffDto } | { status: "error"; error: IpcError }, said: string): boolean {
    if (res.status === "ok") {
      setStaff((all) => all?.map((s) => (s.userId === res.data.userId ? res.data : s)) ?? null);
      setProblem(null);
      setNotice(said);
      setEditing({ kind: "none" });
      return true;
    }
    setNotice(null);
    setProblem(problemOf(res.error, t));
    if (res.error.code === "CONFLICT_VERSION") {
      setEditing({ kind: "none" });
      load();
    }
    return false;
  }

  async function add(e: FormEvent) {
    e.preventDefault();
    const name = draft.name.trim();
    if (!name) return setNewProblem({ field: "displayName", text: t("Enter a name.") });
    const bad = pinProblem(draft.pin, draft.again, t);
    if (bad) return setNewProblem({ field: "pin", text: bad });
    const res = await commands.staffCreate({ displayName: name, role: draft.role, pin: draft.pin });
    if (res.status !== "ok") return setNewProblem(problemOf(res.error, t));
    setStaff((all) => [...(all ?? []), res.data]);
    setNewProblem(null);
    setProblem(null);
    setNotice(t("{name} can now sign in as {role}.", { name: res.data.displayName, role: roleLabel(res.data.role, t).toLowerCase() }));
    setDraft({ name: "", role: "cashier", pin: "", again: "" });
  }

  async function saveEdit(e: FormEvent) {
    e.preventDefault();
    if (editing.kind !== "edit") return;
    const { user, name, role } = editing;
    const res = await commands.staffUpdate({
      userId: user.userId,
      baseVersion: user.version,
      displayName: name.trim() === user.displayName ? null : name,
      role: role === user.role ? null : role,
    });
    settle(res, t("Saved."));
  }

  async function savePin(e: FormEvent) {
    e.preventDefault();
    if (editing.kind !== "pin") return;
    const bad = pinProblem(editing.pin, editing.again, t);
    if (bad) return setProblem({ field: "pin", text: bad });
    const res = await commands.staffResetPin(editing.user.userId, editing.user.version, editing.pin);
    settle(res, t("{name} chooses a new PIN at their next sign-in.", { name: editing.user.displayName }));
  }

  async function deactivate() {
    if (editing.kind !== "deactivate") return;
    const res = await commands.staffDeactivate(editing.user.userId, editing.user.version);
    settle(res, t("{name} can no longer sign in. Their sales and history are kept.", { name: editing.user.displayName }));
  }

  async function reactivate(user: StaffDto) {
    setNotice(null);
    setProblem(null);
    setEditing({ kind: "reactivate", user });
    const res = await commands.staffReactivate(user.userId, user.version);
    settle(res, t("{name} can sign in again with their own PIN.", { name: user.displayName }));
  }

  // The confirmation names the person in bold: the sentence is translated whole and split there.
  const [deactivateBefore, deactivateAfter = ""] = t("Deactivate {name}? They can no longer sign in; their sales and history are kept.").split("{name}");
  const active = staff.filter((s) => s.active);
  const inactive = staff.filter((s) => !s.active);
  const rowProblem = (u: StaffDto) =>
    editing.kind !== "none" && editing.user.userId === u.userId && problem ? (
      <p role="alert" className="field-error inline" id={`staff-${u.userId}-error`}>
        {problem.text}
      </p>
    ) : null;

  return (
    <div className="staff">
      {notice && (
        <p role="status" className="small">
          {notice}
        </p>
      )}
      {problem && editing.kind === "none" && (
        <p role="alert" className="field-error">
          {problem.text}
        </p>
      )}
      <SectionTitle icon={Users}>{t("People who can sign in")}</SectionTitle>
      <table className="sales-table" aria-label={t("Staff")}>
        <thead>
          <tr>
            <th scope="col">{t("Name")}</th>
            <th scope="col">{t("Role")}</th>
            <th scope="col" className="row-actions">
              <span className="visually-hidden">{t("Actions")}</span>
            </th>
          </tr>
        </thead>
        <tbody>
          {active.map((u) => (
            <tr key={u.userId} data-testid="staff-row">
              {editing.kind === "edit" && editing.user.userId === u.userId ? (
                <td colSpan={3}>
                  <form className="setting-edit" aria-label={t("Edit {name}", { name: u.displayName })} onSubmit={(e) => void saveEdit(e)}>
                    <input
                      aria-label={t("Name")}
                      value={editing.name}
                      aria-invalid={problem?.field === "displayName" || undefined}
                      onChange={(e) => setEditing({ ...editing, name: e.target.value })}
                    />
                    <select aria-label={t("Role")} value={editing.role} onChange={(e) => setEditing({ ...editing, role: e.target.value as Role })}>
                      {ROLES.map((r) => (
                        <option key={r} value={r}>
                          {roleLabel(r, t)}
                        </option>
                      ))}
                    </select>
                    <button type="submit">{t("Save")}</button>
                    <button type="button" className="secondary" onClick={() => (setEditing({ kind: "none" }), setProblem(null))}>
                      {t("Cancel")}
                    </button>
                  </form>
                  {rowProblem(u)}
                </td>
              ) : editing.kind === "pin" && editing.user.userId === u.userId ? (
                <td colSpan={3}>
                  <form className="setting-edit" aria-label={t("New PIN for {name}", { name: u.displayName })} onSubmit={(e) => void savePin(e)}>
                    <strong>{u.displayName}</strong>
                    <input
                      aria-label={t("Temporary PIN")}
                      type="password"
                      inputMode="numeric"
                      autoComplete="off"
                      value={editing.pin}
                      onChange={(e) => setEditing({ ...editing, pin: e.target.value })}
                    />
                    <input
                      aria-label={t("Temporary PIN again")}
                      type="password"
                      inputMode="numeric"
                      autoComplete="off"
                      value={editing.again}
                      onChange={(e) => setEditing({ ...editing, again: e.target.value })}
                    />
                    <button type="submit">{t("Set PIN")}</button>
                    <button type="button" className="secondary" onClick={() => (setEditing({ kind: "none" }), setProblem(null))}>
                      {t("Cancel")}
                    </button>
                  </form>
                  <p className="small">{t("Tell them the temporary PIN; the till asks them for a new one when they sign in.")}</p>
                  {rowProblem(u)}
                </td>
              ) : editing.kind === "deactivate" && editing.user.userId === u.userId ? (
                <td colSpan={3}>
                  <div role="group" aria-label={t("Deactivate {name}", { name: u.displayName })} className="setting-edit">
                    <span>
                      {deactivateBefore}
                      <strong>{u.displayName}</strong>
                      {deactivateAfter}
                    </span>
                    <button type="button" className="danger" onClick={() => void deactivate()}>
                      {t("Deactivate")}
                    </button>
                    <button type="button" className="secondary" onClick={() => (setEditing({ kind: "none" }), setProblem(null))}>
                      {t("Keep")}
                    </button>
                  </div>
                  {rowProblem(u)}
                </td>
              ) : (
                <>
                  <th scope="row">
                    {u.displayName}
                    {u.mustChangePin && <span className="tag">{t("new PIN at next sign-in")}</span>}
                  </th>
                  <td>
                    <span className="tag" data-testid="role-badge">
                      {roleLabel(u.role, t)}
                    </span>
                  </td>
                  <td className="row-actions">
                    <button
                      type="button"
                      className="link"
                      aria-label={t("Edit {name}", { name: u.displayName })}
                      onClick={() => (setNotice(null), setProblem(null), setEditing({ kind: "edit", user: u, name: u.displayName, role: u.role }))}
                    >
                      {t("Edit")}
                    </button>{" "}
                    <button
                      type="button"
                      className="link"
                      aria-label={t("Reset PIN for {name}", { name: u.displayName })}
                      onClick={() => (setNotice(null), setProblem(null), setEditing({ kind: "pin", user: u, pin: "", again: "" }))}
                    >
                      {t("Reset PIN")}
                    </button>{" "}
                    <button
                      type="button"
                      className="link"
                      aria-label={t("Deactivate {name}", { name: u.displayName })}
                      onClick={() => (setNotice(null), setProblem(null), setEditing({ kind: "deactivate", user: u }))}
                    >
                      {t("Deactivate")}
                    </button>
                  </td>
                </>
              )}
            </tr>
          ))}
        </tbody>
      </table>
      {inactive.length > 0 && (
        <>
          <SectionTitle icon={UserX}>{t("Deactivated staff")}</SectionTitle>
          <table className="sales-table" aria-label={t("Deactivated staff")} data-testid="staff-inactive">
            <thead>
              <tr>
                <th scope="col">{t("Name")}</th>
                <th scope="col">{t("Role")}</th>
                <th scope="col" className="row-actions">
                  <span className="visually-hidden">{t("Actions")}</span>
                </th>
              </tr>
            </thead>
            <tbody>
              {inactive.map((u) => (
                <tr key={u.userId}>
                  <th scope="row">{u.displayName}</th>
                  <td>
                    <span className="tag">{roleLabel(u.role, t)}</span>
                  </td>
                  <td className="row-actions">
                    <button
                      type="button"
                      className="link"
                      aria-label={t("Reactivate {name}", { name: u.displayName })}
                      aria-describedby={editing.kind === "reactivate" && editing.user.userId === u.userId && problem ? `staff-${u.userId}-error` : undefined}
                      onClick={() => void reactivate(u)}
                    >
                      <UserCheck />
                      {t("Reactivate")}
                    </button>
                    {rowProblem(u)}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </>
      )}

      <SectionTitle icon={UserPlus}>{t("Add a staff member")}</SectionTitle>
      <form className="card sales-filters" aria-label={t("Add a staff member")} onSubmit={(e) => void add(e)}>
        <label>
          {t("Name")}
          <input
            value={draft.name}
            aria-invalid={newProblem?.field === "displayName" || undefined}
            aria-describedby={newProblem?.field === "displayName" ? "staff-new-error" : undefined}
            onChange={(e) => setDraft({ ...draft, name: e.target.value })}
            placeholder={t("e.g. Sam")}
          />
        </label>
        <label>
          {t("Role")}
          <select value={draft.role} onChange={(e) => setDraft({ ...draft, role: e.target.value as Role })}>
            {ROLES.map((r) => (
              <option key={r} value={r}>
                {roleLabel(r, t)}
              </option>
            ))}
          </select>
        </label>
        <label>
          {t("PIN (4–8 digits)")}
          <input
            type="password"
            inputMode="numeric"
            autoComplete="off"
            value={draft.pin}
            aria-invalid={newProblem?.field === "pin" || undefined}
            aria-describedby={newProblem?.field === "pin" ? "staff-new-error" : undefined}
            onChange={(e) => setDraft({ ...draft, pin: e.target.value })}
          />
        </label>
        <label>
          {t("PIN again")}
          <input
            type="password"
            inputMode="numeric"
            autoComplete="off"
            value={draft.again}
            onChange={(e) => setDraft({ ...draft, again: e.target.value })}
          />
        </label>
        <p className="small form-hint">
          {t("The name is shown on the sign-in screen.")} {t(ROLE_COPY[draft.role].help)}
        </p>
        <span className="form-hint">
          <button type="submit">
            <UserPlus />
            {t("Add")}
          </button>
        </span>
      </form>
      {newProblem && (
        <p role="alert" id="staff-new-error" className="field-error">
          {newProblem.text}
        </p>
      )}
    </div>
  );
}
