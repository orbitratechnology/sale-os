import { useCallback, useEffect, useState, type FormEvent } from "react";
import { commands, type IpcError } from "../../ipc";
import type { Role, StaffDto } from "../../ipc/bindings";
import { describeError } from "../shell/errors";
import { SectionTitle } from "../../ui/PanelHeader";
import { UserPlus, Users } from "../../ui/icons";

const ROLES: { role: Role; label: string; help: string }[] = [
  { role: "cashier", label: "Cashier", help: "sell, make returns with a manager's approval, and count the drawer (without seeing the expected amount)." },
  { role: "manager", label: "Manager", help: "do everything a cashier can, approve, edit products and stock, and see reports." },
  { role: "owner", label: "Owner", help: "do everything, including staff, settings and the cloud." },
];
const roleLabel = (r: Role) => ROLES.find((x) => x.role === r)?.label ?? r;

type Field = "displayName" | "pin" | "role";

/** The message for a staff error, and the input it belongs next to. */
function problemOf(e: IpcError): { field: Field | null; text: string } {
  const field = (e.details?.field ?? null) as Field | null;
  const value = e.details?.value ?? null;
  if (e.code === "CONFLICT_VERSION") return { field: null, text: "Someone changed this staff member meanwhile; the list is updated." };
  if (e.code === "VALIDATION_FAILED") {
    if (field === "displayName" && value === "taken") return { field, text: "Another active staff member already has this name." };
    if (field === "displayName" && value) return { field, text: `Names can be at most ${value} characters.` };
    if (field === "displayName") return { field, text: "Enter a name." };
    if (field === "pin" && value === "same") return { field, text: "The new PIN must differ from the current one." };
    if (field === "pin") return { field, text: "A PIN is 4 to 8 digits." };
    if (field === "role") return { field, text: "The business must keep at least one active owner." };
  }
  return { field: null, text: describeError(e) };
}

/** Checked here before sending, so typos get an answer at once; Rust checks again. */
function pinProblem(pin: string, again: string): string | null {
  if (!/^\d{4,8}$/.test(pin)) return "A PIN is 4 to 8 digits.";
  if (pin !== again) return "The two PINs differ.";
  return null;
}

type Editing =
  | { kind: "none" }
  | { kind: "edit"; user: StaffDto; name: string; role: Role }
  | { kind: "pin"; user: StaffDto; pin: string; again: string }
  | { kind: "deactivate"; user: StaffDto };

/**
 * Staff (E05-T06, owner): who can sign in to this till and what they may do. Each person
 * has their own PIN; nobody shares the owner's. An owner can rename, change a role, set a
 * temporary PIN (changed at the next sign-in) and deactivate. Deactivated staff keep their
 * history and can no longer sign in. Rust refuses to leave the business without an owner.
 */
export function StaffPanel() {
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

  if (!staff) return problem ? <p role="alert" className="field-error">{problem.text}</p> : <p className="small">Loading…</p>;

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
    setProblem(problemOf(res.error));
    if (res.error.code === "CONFLICT_VERSION") {
      setEditing({ kind: "none" });
      load();
    }
    return false;
  }

  async function add(e: FormEvent) {
    e.preventDefault();
    const name = draft.name.trim();
    if (!name) return setNewProblem({ field: "displayName", text: "Enter a name." });
    const bad = pinProblem(draft.pin, draft.again);
    if (bad) return setNewProblem({ field: "pin", text: bad });
    const res = await commands.staffCreate({ displayName: name, role: draft.role, pin: draft.pin });
    if (res.status !== "ok") return setNewProblem(problemOf(res.error));
    setStaff((all) => [...(all ?? []), res.data]);
    setNewProblem(null);
    setProblem(null);
    setNotice(`${res.data.displayName} can now sign in as ${roleLabel(res.data.role).toLowerCase()}.`);
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
    settle(res, "Saved.");
  }

  async function savePin(e: FormEvent) {
    e.preventDefault();
    if (editing.kind !== "pin") return;
    const bad = pinProblem(editing.pin, editing.again);
    if (bad) return setProblem({ field: "pin", text: bad });
    const res = await commands.staffResetPin(editing.user.userId, editing.user.version, editing.pin);
    settle(res, `${editing.user.displayName} chooses a new PIN at their next sign-in.`);
  }

  async function deactivate() {
    if (editing.kind !== "deactivate") return;
    const res = await commands.staffDeactivate(editing.user.userId, editing.user.version);
    settle(res, `${editing.user.displayName} can no longer sign in. Their sales and history are kept.`);
  }

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
      <SectionTitle icon={Users}>People who can sign in</SectionTitle>
      <table className="sales-table" aria-label="Staff">
        <thead>
          <tr>
            <th scope="col">Name</th>
            <th scope="col">Role</th>
            <th scope="col" className="row-actions">
              <span className="visually-hidden">Actions</span>
            </th>
          </tr>
        </thead>
        <tbody>
          {active.map((u) => (
            <tr key={u.userId} data-testid="staff-row">
              {editing.kind === "edit" && editing.user.userId === u.userId ? (
                <td colSpan={3}>
                  <form className="setting-edit" aria-label={`Edit ${u.displayName}`} onSubmit={(e) => void saveEdit(e)}>
                    <input
                      aria-label="Name"
                      value={editing.name}
                      aria-invalid={problem?.field === "displayName" || undefined}
                      onChange={(e) => setEditing({ ...editing, name: e.target.value })}
                    />
                    <select aria-label="Role" value={editing.role} onChange={(e) => setEditing({ ...editing, role: e.target.value as Role })}>
                      {ROLES.map((r) => (
                        <option key={r.role} value={r.role}>
                          {r.label}
                        </option>
                      ))}
                    </select>
                    <button type="submit">Save</button>
                    <button type="button" className="secondary" onClick={() => (setEditing({ kind: "none" }), setProblem(null))}>
                      Cancel
                    </button>
                  </form>
                  {rowProblem(u)}
                </td>
              ) : editing.kind === "pin" && editing.user.userId === u.userId ? (
                <td colSpan={3}>
                  <form className="setting-edit" aria-label={`New PIN for ${u.displayName}`} onSubmit={(e) => void savePin(e)}>
                    <strong>{u.displayName}</strong>
                    <input
                      aria-label="Temporary PIN"
                      type="password"
                      inputMode="numeric"
                      autoComplete="off"
                      value={editing.pin}
                      onChange={(e) => setEditing({ ...editing, pin: e.target.value })}
                    />
                    <input
                      aria-label="Temporary PIN again"
                      type="password"
                      inputMode="numeric"
                      autoComplete="off"
                      value={editing.again}
                      onChange={(e) => setEditing({ ...editing, again: e.target.value })}
                    />
                    <button type="submit">Set PIN</button>
                    <button type="button" className="secondary" onClick={() => (setEditing({ kind: "none" }), setProblem(null))}>
                      Cancel
                    </button>
                  </form>
                  <p className="small">Tell them the temporary PIN; the till asks them for a new one when they sign in.</p>
                  {rowProblem(u)}
                </td>
              ) : editing.kind === "deactivate" && editing.user.userId === u.userId ? (
                <td colSpan={3}>
                  <div role="group" aria-label={`Deactivate ${u.displayName}`} className="setting-edit">
                    <span>
                      Deactivate <strong>{u.displayName}</strong>? They can no longer sign in; their sales and history are kept.
                    </span>
                    <button type="button" className="danger" onClick={() => void deactivate()}>
                      Deactivate
                    </button>
                    <button type="button" className="secondary" onClick={() => (setEditing({ kind: "none" }), setProblem(null))}>
                      Keep
                    </button>
                  </div>
                  {rowProblem(u)}
                </td>
              ) : (
                <>
                  <th scope="row">
                    {u.displayName}
                    {u.mustChangePin && <span className="tag">new PIN at next sign-in</span>}
                  </th>
                  <td>
                    <span className="tag" data-testid="role-badge">
                      {roleLabel(u.role)}
                    </span>
                  </td>
                  <td className="row-actions">
                    <button
                      type="button"
                      className="link"
                      aria-label={`Edit ${u.displayName}`}
                      onClick={() => (setNotice(null), setProblem(null), setEditing({ kind: "edit", user: u, name: u.displayName, role: u.role }))}
                    >
                      Edit
                    </button>{" "}
                    <button
                      type="button"
                      className="link"
                      aria-label={`Reset PIN for ${u.displayName}`}
                      onClick={() => (setNotice(null), setProblem(null), setEditing({ kind: "pin", user: u, pin: "", again: "" }))}
                    >
                      Reset PIN
                    </button>{" "}
                    <button
                      type="button"
                      className="link"
                      aria-label={`Deactivate ${u.displayName}`}
                      onClick={() => (setNotice(null), setProblem(null), setEditing({ kind: "deactivate", user: u }))}
                    >
                      Deactivate
                    </button>
                  </td>
                </>
              )}
            </tr>
          ))}
        </tbody>
      </table>
      {inactive.length > 0 && (
        <p className="small" data-testid="staff-inactive">
          Deactivated: {inactive.map((u) => u.displayName).join(", ")}
        </p>
      )}

      <SectionTitle icon={UserPlus}>Add a staff member</SectionTitle>
      <form className="card sales-filters" aria-label="Add a staff member" onSubmit={(e) => void add(e)}>
        <label>
          Name
          <input
            value={draft.name}
            aria-invalid={newProblem?.field === "displayName" || undefined}
            onChange={(e) => setDraft({ ...draft, name: e.target.value })}
            placeholder="e.g. Sam"
          />
        </label>
        <label>
          Role
          <select value={draft.role} onChange={(e) => setDraft({ ...draft, role: e.target.value as Role })}>
            {ROLES.map((r) => (
              <option key={r.role} value={r.role}>
                {r.label}
              </option>
            ))}
          </select>
        </label>
        <label>
          PIN (4–8 digits)
          <input
            type="password"
            inputMode="numeric"
            autoComplete="off"
            value={draft.pin}
            aria-invalid={newProblem?.field === "pin" || undefined}
            onChange={(e) => setDraft({ ...draft, pin: e.target.value })}
          />
        </label>
        <label>
          PIN again
          <input
            type="password"
            inputMode="numeric"
            autoComplete="off"
            value={draft.again}
            onChange={(e) => setDraft({ ...draft, again: e.target.value })}
          />
        </label>
        <p className="small form-hint">
          The name is shown on the sign-in screen. {ROLES.find((r) => r.role === draft.role)?.label}s can{" "}
          {ROLES.find((r) => r.role === draft.role)?.help}
        </p>
        <span className="form-hint">
          <button type="submit">
            <UserPlus />
            Add
          </button>
        </span>
      </form>
      {newProblem && (
        <p role="alert" className="field-error">
          {newProblem.text}
        </p>
      )}
    </div>
  );
}
