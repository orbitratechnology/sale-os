import type { IpcError } from "../../ipc";
import { describeError } from "./errors";
import { useT } from "../../i18n";

export function ErrorPanel({ error }: { error: IpcError }) {
  const t = useT();
  return (
    <section className="card error" role="alert">
      <h2>{t("Something needs attention")}</h2>
      <p>{describeError(error)}</p>
      <p className="mono small">
        {error.code} · {t("ref")} {error.correlationId}
      </p>
    </section>
  );
}
