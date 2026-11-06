import type { Session, Workspace } from "../../ipc";

/**
 * Always visible, never modal (E09-T06): who is signed in, which till, sandbox or live,
 * scanner and sync state. Sync is "Not linked" until Stage 3.
 */
export function StatusBar({ workspace, session, scannerReady }: { workspace: Workspace; session: Session; scannerReady: boolean }) {
  return (
    <footer className="statusbar" aria-label="Status">
      <span>
        {session.displayName} <span className="role-badge">{session.role}</span>
      </span>
      <span>
        {workspace.registerName} · <span className="mono">{workspace.registerCode}</span>
      </span>
      {workspace.mode === "sandbox" ? <span className="sandbox-tag">SANDBOX</span> : <span className="live-tag">LIVE</span>}
      <span>{scannerReady ? "Scanner ready" : "Typing in a field"}</span>
      <span>Not linked · saved on this computer</span>
    </footer>
  );
}
