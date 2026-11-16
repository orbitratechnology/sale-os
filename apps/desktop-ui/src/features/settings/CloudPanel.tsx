import { useCallback, useEffect, useState, type FormEvent } from "react";
import { commands, type IpcError } from "../../ipc";
import type { LinkOptionsDto, SyncStatusDto } from "../../ipc/bindings";
import { describeError } from "../shell/errors";

function when(iso: string | null | undefined): string {
  return iso ? new Date(iso).toLocaleString() : "not yet";
}

/**
 * Settings → Cloud (E17-T08, owner only): where sync stands, and the link wizard. The owner
 * signs in with their cloud account, then either creates the cloud business from this till
 * or joins an existing one (only before this till has live sales or products, because
 * joining replaces its data). Sales never wait for any of this.
 */
export function CloudPanel({ onRelinked }: { onRelinked: () => void }) {
  const [status, setStatus] = useState<SyncStatusDto | null>(null);
  const [error, setError] = useState<IpcError | null>(null);
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [options, setOptions] = useState<LinkOptionsDto | null>(null);
  const [busy, setBusy] = useState(false);
  const [joining, setJoining] = useState<{ businessId: string; name: string } | null>(null);
  const [replace, setReplace] = useState(false);
  const [done, setDone] = useState<string | null>(null);

  const load = useCallback(() => {
    void commands.syncStatus().then((res) => {
      if (res.status !== "ok") return setError(res.error);
      setStatus(res.data);
    });
  }, []);
  useEffect(load, [load]);

  async function signIn(e: FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(null);
    const res = await commands.syncLinkStart(email.trim(), password);
    setBusy(false);
    // The password is not kept once it has been sent.
    setPassword("");
    if (res.status !== "ok") return setError(res.error);
    setOptions(res.data);
  }

  async function complete(mode: "create" | "join", businessId: string | null, replaceExisting: boolean) {
    setBusy(true);
    setError(null);
    const res = await commands.syncLinkComplete(mode, businessId, replaceExisting);
    setBusy(false);
    if (res.status !== "ok") {
      if (res.error.code === "SYNC_REGISTER_IN_USE") setReplace(true);
      return setError(res.error);
    }
    setOptions(null);
    setJoining(null);
    setReplace(false);
    if (res.data.signInAgain) {
      setDone(`Joined. This till is ${res.data.registerCode} now. Sign in with your staff PIN for that business.`);
      onRelinked();
      return;
    }
    setDone(`Linked. This till is ${res.data.registerCode}; what it did offline is being sent now.`);
    load();
  }

  if (!status) return <p aria-busy="true">Loading…</p>;
  if (!status.cloudConfigured) {
    return (
      <div>
        <h2>Cloud</h2>
        <p>No cloud is set up for this app, so this till works on its own. Everything stays on this computer.</p>
      </div>
    );
  }

  const linked = status.linkState === "linked";
  return (
    <div>
      <h2>Cloud</h2>
      {status.linkState === "revoked" && (
        <p role="alert">
          This register was disconnected by the owner. Sales continue on this till and are kept here; link it again to
          sync.
        </p>
      )}
      {status.linkState === "linking" && <p role="status">Linking was interrupted. Sign in again to finish it.</p>}
      {done && <p role="status">{done}</p>}
      {linked ? (
        <dl className="cloud-status">
          <dt>Status</dt>
          <dd>Linked{status.registerCode ? ` as ${status.registerCode}` : ""}</dd>
          <dt>Waiting to send</dt>
          <dd>{status.pending === 0 ? "Nothing" : `${status.pending} change${status.pending === 1 ? "" : "s"}`}</dd>
          <dt>Last sent</dt>
          <dd>{when(status.lastPushOkAt)}</dd>
          <dt>Last received</dt>
          <dd>{when(status.lastPullOkAt)}</dd>
        </dl>
      ) : null}
      {status.clockSkewWarning && (
        <p role="alert">This computer's clock is more than 10 minutes off. Correct the date and time.</p>
      )}
      {linked && (
        <button type="button" onClick={() => void commands.syncRetryNow().then(load)}>
          Send now
        </button>
      )}
      {error && (
        <p role="alert" className="field-error">
          {describeError(error)}
        </p>
      )}
      {!linked && !options && (
        <form onSubmit={(e) => void signIn(e)} aria-label="Cloud sign-in" noValidate>
          <label>
            Cloud account email
            <input type="email" autoComplete="username" value={email} onChange={(e) => setEmail(e.target.value)} required />
          </label>
          <label>
            Password
            <input type="password" autoComplete="current-password" value={password} onChange={(e) => setPassword(e.target.value)} required />
          </label>
          <button type="submit" disabled={busy || !email.trim() || !password}>
            Sign in to link
          </button>
        </form>
      )}
      {options && !joining && (
        <section aria-label="Link this till">
          <h3>Create</h3>
          <p>Use this till's business, products and staff as the cloud business. Nothing here changes.</p>
          <button type="button" disabled={busy} onClick={() => void complete("create", null, false)}>
            {options.ownBusinessInCloud ? "Finish linking this till's business" : "Create the cloud business from this till"}
          </button>
          {replace && (
            <p>
              Another device is linked to this till in the cloud.{" "}
              <button type="button" disabled={busy} onClick={() => void complete("create", null, true)}>
                Replace it with this computer
              </button>
            </p>
          )}
          <h3>Join</h3>
          {!options.mayJoin && (
            <p id="join-blocked">
              Joining replaces this till's data with the other business's, so it is only possible before this till has live
              sales or products.
            </p>
          )}
          <ul>
            {options.businesses.map((b) => (
              <li key={b.businessId}>
                {b.name}{" "}
                <button
                  type="button"
                  className="secondary"
                  disabled={busy || !options.mayJoin}
                  aria-describedby={options.mayJoin ? undefined : "join-blocked"}
                  onClick={() => setJoining(b)}
                >
                  Join {b.name}
                </button>
              </li>
            ))}
          </ul>
        </section>
      )}
      {joining && (
        <div role="alertdialog" aria-label="Join a business">
          <p>
            Joining replaces everything on this till with {joining.name}'s products, settings and staff. Its first sale gets
            its own register number.
          </p>
          <button type="button" disabled={busy} onClick={() => void complete("join", joining.businessId, false)}>
            Join {joining.name}
          </button>{" "}
          <button type="button" className="secondary" disabled={busy} onClick={() => setJoining(null)}>
            Cancel
          </button>
        </div>
      )}
    </div>
  );
}
