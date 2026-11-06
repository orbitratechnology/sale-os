import { useCallback, useEffect, useState, type FormEvent } from "react";
import { commands, type IpcError, type Session, type SignInUser, type Workspace } from "../../ipc";
import { describeError } from "../shell/errors";
import { PinPad } from "./PinPad";

/**
 * Sign-in (E05-T02/T06): pick your name, enter your PIN. Wrong PINs show the tries left;
 * a temporary lock shows a countdown. After an owner reset, a new PIN is required first.
 */
export function SignIn({
  workspace,
  session,
  onSignedIn,
}: {
  workspace: Workspace;
  session: Session | null;
  onSignedIn: (s: Session) => void;
}) {
  if (session?.mustChangePin) return <ChangePin session={session} onChanged={onSignedIn} />;
  return <PickAndPin workspace={workspace} onSignedIn={onSignedIn} />;
}

function PickAndPin({ workspace, onSignedIn }: { workspace: Workspace; onSignedIn: (s: Session) => void }) {
  const [users, setUsers] = useState<SignInUser[] | null>(null);
  const [user, setUser] = useState<SignInUser | null>(null);
  const [pin, setPin] = useState("");
  const [error, setError] = useState<IpcError | null>(null);
  const [lockedUntil, setLockedUntil] = useState<number | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    void commands.authListUsers().then((res) => {
      if (res.status === "ok") {
        setUsers(res.data);
        if (res.data.length === 1) setUser(res.data[0] ?? null);
      } else setError(res.error);
    });
  }, []);

  useEffect(() => {
    if (lockedUntil === null) return;
    const t = setInterval(() => setNow(Date.now()), 250);
    return () => clearInterval(t);
  }, [lockedUntil]);

  const secondsLeft = lockedUntil === null ? 0 : Math.max(0, Math.ceil((lockedUntil - now) / 1000));
  const locked = secondsLeft > 0;

  const submit = useCallback(async () => {
    if (!user || busy || pin.length < 4) return;
    setBusy(true);
    const res = await commands.authLogin(user.userId, pin);
    setBusy(false);
    setPin("");
    if (res.status === "ok") {
      onSignedIn(res.data);
      return;
    }
    setError(res.error);
    const retryAt = res.error.details?.retryAtMs;
    if (res.error.code === "AUTH_LOCKED_OUT" && retryAt) {
      setLockedUntil(parseInt(retryAt, 10));
      setNow(Date.now());
    }
  }, [user, pin, busy, onSignedIn]);

  return (
    <main className="signin">
      <h1>{workspace.businessName}</h1>
      <p className="small">
        {workspace.registerName} · {workspace.registerCode}
        {workspace.mode === "sandbox" && <span className="sandbox-tag">SANDBOX</span>}
      </p>
      {!user ? (
        <section aria-label="Who is signing in?">
          <h2>Who is signing in?</h2>
          <ul className="user-grid">
            {users?.map((u) => (
              <li key={u.userId}>
                <button type="button" className="user-tile" onClick={() => setUser(u)}>
                  <span className="user-name">{u.displayName}</span>
                  <span className="role-badge">{u.role}</span>
                </button>
              </li>
            ))}
          </ul>
        </section>
      ) : (
        <section aria-label={`PIN for ${user.displayName}`} className="pin-panel">
          <h2>
            {user.displayName} <span className="role-badge">{user.role}</span>
          </h2>
          <PinPad value={pin} onChange={setPin} onSubmit={() => void submit()} disabled={busy || locked} label="PIN" />
          <div aria-live="polite" className="pin-status">
            {busy ? (
              <p className="small">Checking…</p>
            ) : locked ? (
              <p role="alert" className="field-error">
                Too many wrong PINs. Try again in {secondsLeft} s.
              </p>
            ) : (
              error && (
                <p role="alert" className="field-error">
                  {describeError(error)}
                </p>
              )
            )}
          </div>
          {(users?.length ?? 0) > 1 && (
            <button
              type="button"
              className="secondary"
              onClick={() => {
                setUser(null);
                setPin("");
                setError(null);
                setLockedUntil(null);
              }}
            >
              Not {user.displayName}?
            </button>
          )}
        </section>
      )}
    </main>
  );
}

function ChangePin({ session, onChanged }: { session: Session; onChanged: (s: Session) => void }) {
  const [current, setCurrent] = useState("");
  const [next, setNext] = useState("");
  const [again, setAgain] = useState("");
  const [problem, setProblem] = useState<string | null>(null);

  async function submit(e: FormEvent) {
    e.preventDefault();
    if (!/^\d{4,8}$/.test(next)) return setProblem("The new PIN must be 4 to 8 digits.");
    if (next !== again) return setProblem("The two new PINs do not match.");
    const res = await commands.authChangePin(current, next);
    if (res.status === "ok") onChanged(res.data);
    else setProblem(describeError(res.error));
  }

  return (
    <main className="signin">
      <form className="card setup-form" onSubmit={(e) => void submit(e)}>
        <h2>Choose a new PIN, {session.displayName}</h2>
        <p className="small">Your PIN was reset. Choose a new one only you know.</p>
        <label>
          Temporary PIN
          <input type="password" inputMode="numeric" autoFocus value={current} onChange={(e) => setCurrent(e.target.value)} />
        </label>
        <label>
          New PIN
          <input type="password" inputMode="numeric" value={next} onChange={(e) => setNext(e.target.value)} />
        </label>
        <label>
          New PIN again
          <input type="password" inputMode="numeric" value={again} onChange={(e) => setAgain(e.target.value)} />
        </label>
        {problem && (
          <p role="alert" className="field-error">
            {problem}
          </p>
        )}
        <div className="actions">
          <button type="submit">Save PIN</button>
        </div>
      </form>
    </main>
  );
}
