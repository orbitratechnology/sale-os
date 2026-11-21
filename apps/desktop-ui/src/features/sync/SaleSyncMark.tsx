import { useEffect, useState } from "react";
import { commands } from "../../ipc";
import { saleSyncText } from "./syncText";
import { useT } from "../../i18n";

/** How often a pending sale asks again whether it reached the cloud. */
const POLL_MS = 3_000;

/**
 * `Saved on this device · Sync pending` until the sale's event is applied in the cloud, then
 * `Synced` (AT-13). Asks Rust; stops asking once the sale is synced.
 */
export function SaleSyncMark({ saleId }: { saleId: string }) {
  const t = useT();
  const [state, setState] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    let timer: number | undefined;
    const ask = () =>
      void commands.saleSyncState(saleId).then((res) => {
        if (!live || res.status !== "ok") return;
        setState(res.data.state);
        if (res.data.state === "pending") timer = window.setTimeout(ask, POLL_MS);
      });
    ask();
    return () => {
      live = false;
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, [saleId]);
  return <span data-state={state ?? "unknown"}>{state === null ? t("Saved on this device") : saleSyncText(state, t)}</span>;
}
