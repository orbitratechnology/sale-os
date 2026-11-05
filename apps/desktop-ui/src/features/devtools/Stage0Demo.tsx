import { useState } from "react";
import { commands, type DemoCart, type DemoProduct, type DemoSaleResult, type IpcError } from "../../ipc";
import { useScanCapture } from "../scan/useScanCapture";

/**
 * STAGE 0 SPIKE (E00-T06): the vertical slice in the real app.
 * Set up a product, scan it through the real keyboard-wedge hook, complete a cash sale
 * atomically in Rust, and print to the virtual printer. Removed when checkout lands.
 */
export function Stage0Demo() {
  const [product, setProduct] = useState<DemoProduct | null>(null);
  const [cart, setCart] = useState<DemoCart | null>(null);
  const [cash, setCash] = useState("500");
  const [sale, setSale] = useState<DemoSaleResult | null>(null);
  const [error, setError] = useState<IpcError | null>(null);

  useScanCapture((scan) => {
    void commands.devDemoScan(scan.raw, scan.scanEventId).then((res) => {
      if (res.status === "ok") {
        setCart(res.data);
        setSale(null);
        setError(null);
      } else setError(res.error);
    });
  });

  async function setUp() {
    const res = await commands.devDemoSetup();
    if (res.status === "ok") {
      setProduct(res.data);
      setError(null);
      const c = await commands.devDemoCart();
      if (c.status === "ok") setCart(c.data);
    } else setError(res.error);
  }

  /** A virtual keyboard-wedge scanner: types the barcode at 5 ms per key, then Enter. */
  function simulateScan(code: string) {
    (document.activeElement as HTMLElement | null)?.blur();
    [...code, "Enter"].forEach((key, i) => {
      setTimeout(() => window.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true })), i * 5);
    });
  }

  async function complete() {
    const res = await commands.devDemoCompleteSale(cash);
    if (res.status === "ok") {
      setSale(res.data);
      setError(null);
      const c = await commands.devDemoCart();
      if (c.status === "ok") setCart(c.data);
    } else setError(res.error);
  }

  return (
    <section className="card" aria-label="Stage 0 vertical slice">
      <h2>Stage 0 vertical slice</h2>
      <div className="row">
        <button type="button" onClick={() => void setUp()}>
          1. Set up demo product
        </button>
        <button type="button" disabled={!product} onClick={() => product && simulateScan(product.barcode)}>
          2. Scan (virtual wedge scanner)
        </button>
        <label>
          Cash received (minor units)
          <input value={cash} onChange={(e) => setCash(e.target.value)} inputMode="numeric" />
        </label>
        <button type="button" disabled={!cart?.lines.length} onClick={() => void complete()}>
          3. Complete cash sale
        </button>
      </div>

      {product && (
        <p className="small">
          {product.name} · barcode <span className="mono">{product.barcode}</span> · {product.priceFormatted}
        </p>
      )}

      {cart && cart.lines.length > 0 && (
        <table className="cart">
          <thead>
            <tr>
              <th>Item</th>
              <th>Qty</th>
              <th>Total</th>
            </tr>
          </thead>
          <tbody>
            {cart.lines.map((l, i) => (
              <tr key={i}>
                <td>{l.name}</td>
                <td>{l.quantity}</td>
                <td>{l.lineTotalFormatted}</td>
              </tr>
            ))}
          </tbody>
          <tfoot>
            <tr>
              <td colSpan={2}>Total due</td>
              <td className="total">{cart.totalFormatted}</td>
            </tr>
          </tfoot>
        </table>
      )}

      {sale && (
        <div aria-live="polite">
          <p className="total">Change {sale.changeFormatted}</p>
          <p>
            Receipt <strong className="mono">{sale.receiptNumber}</strong> · total {sale.totalFormatted} · Saved on this
            device · Not linked (outbox event #{sale.outboxSequence})
          </p>
          <p className="small mono">Virtual printer: {sale.print}</p>
        </div>
      )}

      {error && (
        <p role="alert" className="mono small">
          {error.code}: {error.message}
        </p>
      )}
    </section>
  );
}
