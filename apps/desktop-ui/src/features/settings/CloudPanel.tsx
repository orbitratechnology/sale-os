import { useCallback, useEffect, useState, type FormEvent } from "react";
import { commands, type IpcError } from "../../ipc";
import type { LinkOptionsDto, RepairQueueDto, SyncStatusDto } from "../../ipc/bindings";
import { describeError } from "../shell/errors";
import { indicatorText, refusalText } from "../sync/syncText";
import { SectionTitle } from "../../ui/PanelHeader";
import { Dialog } from "../checkout/Dialog";
import { CloudOff, Link, Lock, RefreshCw, TriangleExclamationPoint } from "../../ui/icons";
import { useT, type T } from "../../i18n";

function when(iso: string | null | undefined, t: T): string {
  return iso ? new Date(iso).toLocaleString() : t("not yet");
}

/**
 * Settings → Cloud (E17-T08, owner only): where sync stands, and the link wizard. The owner
 * signs in with their cloud account, then either creates the cloud business from this till
 * or joins an existing one (only before this till has live sales or products, because
 * joining replaces its data). Sales never wait for any of this.
 */
export function CloudPanel({ onRelinked }: { onRelinked: () => void }) {
  const t = useT();
  const [status, setStatus] = useState<SyncStatusDto | null>(null);
  const [error, setError] = useState<IpcError | null>(null);
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [options, setOptions] = useState<LinkOptionsDto | null>(null);
  const [busy, setBusy] = useState(false);
  const [joining, setJoining] = useState<{ businessId: string; name: string } | null>(null);
  const [replace, setReplace] = useState(false);
  const [done, setDone] = useState<string | null>(null);
  const [queue, setQueue] = useState<RepairQueueDto | null>(null);
  const [payload, setPayload] = useState<{ eventId: string; json: string } | null>(null);
  const [exported, setExported] = useState<string | null>(null);

  const load = useCallback(() => {
    void commands.syncStatus().then((res) => {
      if (res.status !== "ok") return setError(res.error);
      setStatus(res.data);
      if (res.data.rejected + res.data.conflicts > 0) {
        void commands.syncRepairQueue().then((q) => {
          if (q.status === "ok") setQueue(q.data);
        });
      } else setQueue(null);
    });
  }, []);

  async function viewPayload(eventId: string) {
    if (payload?.eventId === eventId) return setPayload(null);
    const res = await commands.syncRepairAction("view_payload", eventId);
    if (res.status !== "ok") return setError(res.error);
    setPayload({ eventId, json: res.data.payloadJson ?? "" });
  }

  async function exportSupport() {
    const res = await commands.syncRepairAction("export_support", null);
    if (res.status !== "ok") return setError(res.error);
    setExported(res.data.savedFileName ? t("Saved {file}. Send it to support.", { file: res.data.savedFileName }) : null);
  }
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
      setDone(t("Joined. This till is {code} now. Sign in with your staff PIN for that business.", { code: res.data.registerCode }));
      onRelinked();
      return;
    }
    setDone(t("Linked. This till is {code}; what it did offline is being sent now.", { code: res.data.registerCode }));
    load();
  }

  if (!status) return <p aria-busy="true">{t("Loading…")}</p>;
  if (!status.cloudConfigured) {
    return (
      <div className="empty-state">
        <span className="empty-icon">
          <CloudOff />
        </span>
        <p>{t("No cloud is set up for this app, so this till works on its own. Everything stays on this computer.")}</p>
      </div>
    );
  }

  const linked = status.linkState === "linked";
  return (
    <div className="panel-stack">
      {status.linkState === "revoked" && (
        <p role="alert">
          {t("This register was disconnected by the owner. Sales continue on this till and are kept here; link it again to sync.")}
        </p>
      )}
      {status.linkState === "linking" && <p role="status">{t("Linking was interrupted. Sign in again to finish it.")}</p>}
      {done && <p role="status">{done}</p>}
      {linked ? (
        <dl className="cloud-status">
          <dt>{t("Status")}</dt>
          <dd>
            {status.registerCode ? t("Linked as {code}", { code: status.registerCode }) : t("Linked")} · {indicatorText(status, t)}
          </dd>
          <dt>{t("Conflicts")}</dt>
          <dd>{status.conflicts === 0 ? t("None") : status.conflicts}</dd>
          <dt>{t("Refused by the cloud")}</dt>
          <dd>{status.rejected === 0 ? t("None") : status.rejected}</dd>
          <dt>{t("Waiting to send")}</dt>
          <dd>
            {status.pending === 0
              ? t("Nothing")
              : status.pending === 1
                ? t("{n} change", { n: status.pending })
                : t("{n} changes", { n: status.pending })}
          </dd>
          <dt>{t("Last sent")}</dt>
          <dd>{when(status.lastPushOkAt, t)}</dd>
          <dt>{t("Last received")}</dt>
          <dd>{when(status.lastPullOkAt, t)}</dd>
        </dl>
      ) : null}
      {status.clockSkewWarning && (
        <p role="alert">{t("This computer's clock is more than 10 minutes off. Correct the date and time.")}</p>
      )}
      {linked && (
        <p className="button-row">
          <button type="button" onClick={() => void commands.syncRetryNow().then(load)}>
            <RefreshCw />
            {t("Send now")}
          </button>
          <button type="button" className="secondary" onClick={() => void exportSupport()}>
            {t("Export for support")}
          </button>
        </p>
      )}
      {exported && <p role="status">{exported}</p>}
      {queue && queue.events.length > 0 && (
        <section aria-label={t("Refused by the cloud")}>
          <SectionTitle icon={TriangleExclamationPoint}>{t("Refused by the cloud")}</SectionTitle>
          <ul className="repair-queue">
            {queue.events.map((e) => (
              <li key={e.eventId}>
                <p>
                  <strong>{e.eventType}</strong> #{e.localSequence} · <span className="mono">{e.code ?? t("unknown")}</span>
                  {e.kind === "blocked" ? ` · ${t("holding up later changes")}` : ""}
                </p>
                <p>{refusalText(e.code, t)}</p>
                <button type="button" className="secondary" onClick={() => void viewPayload(e.eventId)}>
                  {payload?.eventId === e.eventId ? t("Hide what was sent") : t("View what was sent")}
                </button>
                {payload?.eventId === e.eventId && (
                  <pre className="payload" aria-label={t("What was sent for #{n}", { n: e.localSequence })}>
                    {payload.json}
                  </pre>
                )}
              </li>
            ))}
          </ul>
        </section>
      )}
      {queue && queue.conflicts.length > 0 && (
        <section aria-label={t("Conflicts")}>
          <SectionTitle icon={TriangleExclamationPoint}>{t("Conflicts")}</SectionTitle>
          <p>
            {t(
              "These were changed here and in the cloud at the same time. The cloud kept its version; settling them comes with the conflict screen.",
            )}
          </p>
          <ul>
            {queue.conflicts.map((c) => (
              <li key={c.conflictId}>
                {c.aggregateType} ·{" "}
                {t("edited here from version {local}, the cloud is at {server}", { local: c.localBaseVersion, server: c.serverVersion })}
              </li>
            ))}
          </ul>
        </section>
      )}
      {error && (
        <p role="alert" className="field-error">
          {describeError(error)}
        </p>
      )}
      {!linked && !options && (
        <form onSubmit={(e) => void signIn(e)} aria-label={t("Cloud sign-in")} noValidate className="stack-form">
          <SectionTitle icon={Lock}>{t("Sign in to your cloud account")}</SectionTitle>
          <p className="small">
            {t(
              "Linking sends this till's sales, products and stock to your online account, and keeps them in step with your other tills. Selling never waits for the internet.",
            )}
          </p>
          <label>
            {t("Cloud account email")}
            <input type="email" autoComplete="username" value={email} onChange={(e) => setEmail(e.target.value)} required />
          </label>
          <label>
            {t("Password")}
            <input type="password" autoComplete="current-password" value={password} onChange={(e) => setPassword(e.target.value)} required />
          </label>
          <button type="submit" disabled={busy || !email.trim() || !password}>
            <Link />
            {t("Sign in to link")}
          </button>
        </form>
      )}
      {options && !joining && (
        <section aria-label={t("Link this till")}>
          <h3>{t("Create a new cloud business")}</h3>
          <p>{t("Use this till's business, products and staff as the cloud business. Nothing here changes.")}</p>
          <button type="button" disabled={busy} onClick={() => void complete("create", null, false)}>
            {options.ownBusinessInCloud ? t("Finish linking this till's business") : t("Create the cloud business from this till")}
          </button>
          {replace && (
            <p>
              {t("Another device is linked to this till in the cloud.")}{" "}
              <button type="button" disabled={busy} onClick={() => void complete("create", null, true)}>
                {t("Replace it with this computer")}
              </button>
            </p>
          )}
          <h3>{t("Or join an existing one")}</h3>
          {!options.mayJoin && (
            <p id="join-blocked">
              {t(
                "Joining replaces this till's data with the other business's, so it is only possible before this till has live sales or products.",
              )}
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
                  {t("Join {name}", { name: b.name })}
                </button>
              </li>
            ))}
          </ul>
        </section>
      )}
      {joining && (
        <Dialog
          title={t("Join a business")}
          alert
          safeFocus
          danger
          confirmLabel={t("Join {name}", { name: joining.name })}
          confirmDisabled={busy}
          onConfirm={() => void complete("join", joining.businessId, false)}
          onCancel={() => setJoining(null)}
        >
          <p>
            {t("Joining replaces everything on this till with {name}'s products, settings and staff. Its first sale gets its own register number.", {
              name: joining.name,
            })}
          </p>
        </Dialog>
      )}
    </div>
  );
}
