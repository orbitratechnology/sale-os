import { useCallback, useEffect, useState } from "react";
import { commands, type BootstrapState, type IpcError, type Result, type Session } from "./ipc";
import { SetupWizard } from "./features/setup/SetupWizard";
import { SignIn } from "./features/auth/SignIn";
import { Checkout } from "./features/checkout/Checkout";
import { ErrorPanel } from "./features/shell/ErrorPanel";

/**
 * The app shell: first run → sign-in → checkout. Rust holds the session; this component only
 * mirrors it, and goes back to sign-in whenever a command reports `SESSION_LOCKED`.
 */
export function App() {
  const [state, setState] = useState<BootstrapState | null>(null);
  const [error, setError] = useState<IpcError | null>(null);

  const apply = useCallback((res: Result<BootstrapState>) => {
    if (res.status === "ok") {
      setState(res.data);
      setError(null);
    } else setError(res.error);
  }, []);

  const refresh = useCallback(() => void commands.appBootstrapState().then(apply), [apply]);

  useEffect(() => {
    let live = true;
    void commands.appBootstrapState().then((res) => live && apply(res));
    return () => {
      live = false;
    };
  }, [apply]);

  const signedIn = useCallback((session: Session) => {
    setState((s) => (s ? { ...s, session } : s));
  }, []);
  const signedOut = useCallback(() => {
    setState((s) => (s ? { ...s, session: null } : s));
  }, []);

  if (error) {
    return (
      <main className="shell">
        <ErrorPanel error={error} />
      </main>
    );
  }
  if (!state) return <main className="shell" aria-busy="true" />;
  if (!state.setupComplete || !state.workspace) return <SetupWizard onDone={refresh} />;
  if (!state.session || state.session.mustChangePin) {
    return <SignIn workspace={state.workspace} session={state.session} onSignedIn={signedIn} />;
  }
  return <Checkout workspace={state.workspace} session={state.session} onSignedOut={signedOut} />;
}
