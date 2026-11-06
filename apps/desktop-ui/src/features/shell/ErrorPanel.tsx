import type { IpcError } from "../../ipc";
import { describeError } from "./errors";

export function ErrorPanel({ error }: { error: IpcError }) {
  return (
    <section className="card error" role="alert">
      <h2>Something needs attention</h2>
      <p>{describeError(error)}</p>
      <p className="mono small">
        {error.code} · ref {error.correlationId}
      </p>
    </section>
  );
}
