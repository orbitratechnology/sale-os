import type {
  CloudGroupByDto,
  CloudReportRowDto,
  CloudSalesReportDto,
  TenderKind,
} from "../../ipc/bindings";
import { formatMoney } from "../../lib/money";
import { TENDER_LABEL } from "../sales/labels";
import { clock, cloudLabel } from "./cloudLabel";

export const GROUPS: {
  value: CloudGroupByDto;
  label: string;
  column: string;
}[] = [
  { value: "day", label: "By day", column: "Day" },
  { value: "register", label: "By register", column: "Register" },
  { value: "tender", label: "By payment", column: "Payment" },
];

function name(r: CloudReportRowDto, groupBy: string): string {
  if (groupBy === "tender") return TENDER_LABEL[r.key as TenderKind] ?? r.label;
  return r.label;
}

/**
 * All registers, from the cloud (E14-T05). Says how complete the figures are, or that they
 * are the last ones fetched while the cloud is out of reach.
 */
export function CloudReport({ r }: { r: CloudSalesReportDto }) {
  const money = (m: string) => formatMoney(m, { exponent: r.currencyExponent });
  const neg = (m: string) => (m === "0" ? money("0") : `-${money(m)}`);
  const label = cloudLabel(r);
  const tender = r.groupBy === "tender";
  const column = GROUPS.find((g) => g.value === r.groupBy)?.column ?? "";
  return (
    <>
      <p
        className={label.stale ? "scope-badge stale" : "scope-badge"}
        data-testid="cloud-label"
        role={label.stale ? "alert" : undefined}
      >
        All registers (cloud) · {label.text}
      </p>
      <table
        className="sales-table"
        aria-label={`All registers ${GROUPS.find((g) => g.value === r.groupBy)?.label.toLowerCase() ?? ""}`}
      >
        <thead>
          <tr>
            <th scope="col">{column}</th>
            <th scope="col" className="num">
              {tender ? "Taken" : "Sales"}
            </th>
            {!tender && (
              <>
                <th scope="col" className="num">
                  Discounts
                </th>
                <th scope="col" className="num">
                  Tax
                </th>
              </>
            )}
            <th scope="col" className="num">
              Refunds
            </th>
            <th scope="col" className="num">
              Net
            </th>
          </tr>
        </thead>
        <tbody>
          {r.rows.map((x) => (
            <tr key={x.key} data-testid="cloud-row">
              <th scope="row">{name(x, r.groupBy)}</th>
              <td className="num">
                {money(x.salesMinor)} ({x.salesCount})
              </td>
              {!tender && (
                <>
                  <td className="num">{neg(x.discountMinor)}</td>
                  <td className="num">{money(x.taxMinor)}</td>
                </>
              )}
              <td className="num">
                {x.returnsCount
                  ? `${neg(x.refundsMinor)} (${x.returnsCount})`
                  : neg(x.refundsMinor)}
              </td>
              <td className="num">{money(x.netMinor)}</td>
            </tr>
          ))}
        </tbody>
        {r.rows.length > 0 && (
          <tfoot>
            <tr data-testid="cloud-total">
              <th scope="row">Total</th>
              <td className="num">
                {money(r.total.salesMinor)} ({r.total.salesCount})
              </td>
              {!tender && (
                <>
                  <td className="num">{neg(r.total.discountMinor)}</td>
                  <td className="num">{money(r.total.taxMinor)}</td>
                </>
              )}
              <td className="num">{neg(r.total.refundsMinor)}</td>
              <td className="num">
                <strong>{money(r.total.netMinor)}</strong>
              </td>
            </tr>
          </tfoot>
        )}
      </table>
      {r.rows.length === 0 && (
        <p className="small">No sales from any register in this period.</p>
      )}
      {r.registers.length > 0 && (
        <details>
          <summary>Registers</summary>
          <ul className="small" aria-label="Registers">
            {r.registers.map((g) => (
              <li key={g.code}>
                <strong>{g.code}</strong> {g.name} ·{" "}
                {g.current
                  ? "up to date"
                  : g.pendingKnown
                    ? "has data not yet sent"
                    : g.lastAckAt
                      ? `last heard from ${clock(g.lastAckAt)}`
                      : "not heard from yet"}
              </li>
            ))}
          </ul>
        </details>
      )}
      <p className="small">
        Live sales only; refunds are of returns made in the period, at any
        register.
      </p>
    </>
  );
}
