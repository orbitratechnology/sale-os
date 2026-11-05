import { useEffect, useState } from "react";
import { commands, type AppInfo, type IpcError } from "./ipc";
import { Stage0Demo } from "./features/devtools/Stage0Demo";

/** Stage 0 shell: shows build and database status. Checkout replaces this in Stage 1. */
export function App() {
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [error, setError] = useState<IpcError | null>(null);

  useEffect(() => {
    void commands.appInfo().then((res) => {
      if (res.status === "ok") setInfo(res.data);
      else setError(res.error);
    });
  }, []);

  return (
    <main className="shell">
      <header>
        <h1>Sale OS</h1>
        {info && (
          <span className="badge">
            v{info.version}
            {info.debugBuild ? " · debug" : ""}
          </span>
        )}
      </header>

      {error && <ErrorPanel error={error} />}

      {info && (
        <section className="card" aria-label="Local database">
          <h2>Local database</h2>
          {info.database.state === "ready" ? (
            <dl>
              <dt>Status</dt>
              <dd>Ready</dd>
              <dt>Schema version</dt>
              <dd>{info.database.schemaVersion}</dd>
              <dt>File</dt>
              <dd className="mono">{info.database.path}</dd>
            </dl>
          ) : (
            <ErrorPanel error={info.database.error} />
          )}
        </section>
      )}

      <Stage0Demo />
    </main>
  );
}

const ERROR_COPY: Partial<Record<IpcError["code"], string>> = {
  STORE_SCHEMA_TOO_NEW: "This database was created by a newer version of Sale OS. Update the app to continue.",
  STORE_IO: "Sale OS cannot read or write its database. Check disk space and that the data folder is on this computer.",
  STORE_CORRUPT: "The local database looks damaged. Do not delete it; contact support to restore from a backup.",
};

function ErrorPanel({ error }: { error: IpcError }) {
  return (
    <section className="card error" role="alert">
      <h2>Something needs attention</h2>
      <p>{ERROR_COPY[error.code] ?? "An unexpected error occurred."}</p>
      <p className="mono small">
        {error.code} · ref {error.correlationId}
      </p>
    </section>
  );
}
