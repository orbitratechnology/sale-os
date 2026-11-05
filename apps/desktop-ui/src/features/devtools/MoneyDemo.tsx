import { useState, type FormEvent } from "react";
import { commands, type IpcError, type MoneyAddResult } from "../../ipc";

/**
 * Stage 0 demo (E00-T06 precursor): the UI sends minor-unit strings to Rust, Rust adds them
 * with pos-domain and formats the result. The UI does no arithmetic on money.
 */
export function MoneyDemo() {
  const [a, setA] = useState("1999");
  const [b, setB] = useState("1");
  const [currency, setCurrency] = useState("USD");
  const [result, setResult] = useState<MoneyAddResult | null>(null);
  const [error, setError] = useState<IpcError | null>(null);

  async function submit(e: FormEvent) {
    e.preventDefault();
    const res = await commands.devMoneyAdd(a, b, currency);
    if (res.status === "ok") {
      setResult(res.data);
      setError(null);
    } else {
      setResult(null);
      setError(res.error);
    }
  }

  return (
    <section className="card" aria-label="Money demo">
      <h2>Domain round-trip</h2>
      <form onSubmit={(e) => void submit(e)} className="row">
        <label>
          A (minor units)
          <input value={a} onChange={(e) => setA(e.target.value)} inputMode="numeric" />
        </label>
        <label>
          B (minor units)
          <input value={b} onChange={(e) => setB(e.target.value)} inputMode="numeric" />
        </label>
        <label>
          Currency
          <input value={currency} onChange={(e) => setCurrency(e.target.value)} maxLength={3} />
        </label>
        <button type="submit">Add in Rust</button>
      </form>
      {result && (
        <p className="total" aria-live="polite">
          {result.formatted} <span className="mono small">({result.sumMinor} minor units)</span>
        </p>
      )}
      {error && (
        <p role="alert" className="mono small">
          {error.code}: {error.message}
        </p>
      )}
    </section>
  );
}
