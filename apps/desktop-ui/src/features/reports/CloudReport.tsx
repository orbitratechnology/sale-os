import type {
  CloudGroupByDto,
  CloudReportRowDto,
  CloudSalesReportDto,
  TenderKind,
} from "../../ipc/bindings";
import { useT, type T } from "../../i18n";
import { formatMoney } from "../../lib/money";
import { TENDER_LABEL } from "../sales/labels";
import { clock, cloudLabel } from "./cloudLabel";

/** The groupings, in order; English, translated at render with `t`. */
const GROUP_COPY: Record<CloudGroupByDto, { label: string; column: string }> = {
  day: { label: "By day", column: "Day" },
  register: { label: "By register", column: "Register" },
  tender: { label: "By payment", column: "Payment" },
};

/** `label` and `column` are English: show them through `t`. */
export const GROUPS: { value: CloudGroupByDto; label: string; column: string }[] = (
  Object.keys(GROUP_COPY) as CloudGroupByDto[]
).map((value) => ({ value, ...GROUP_COPY[value] }));

function name(r: CloudReportRowDto, groupBy: string, t: T): string {
  if (groupBy === "tender") {
    const tender = TENDER_LABEL[r.key as TenderKind] as string | undefined;
    return tender === undefined ? r.label : t(tender);
  }
  return r.label;
}

/** The table's accessible name. */
function tableName(groupBy: string, t: T): string {
  switch (groupBy) {
    case "day":
      return t("All registers by day");
    case "register":
      return t("All registers by register");
    case "tender":
      return t("All registers by payment");
    default:
      return "All registers ";
  }
}

/**
 * All registers, from the cloud (E14-T05). Says how complete the figures are, or that they
 * are the last ones fetched while the cloud is out of reach.
 */
export function CloudReport({ r }: { r: CloudSalesReportDto }) {
  const money = (m: string) => formatMoney(m, { exponent: r.currencyExponent });
  const neg = (m: string) => (m === "0" ? money("0") : `-${money(m)}`);
  const t = useT();
  const label = cloudLabel(r, undefined, t);
  const tender = r.groupBy === "tender";
  const column = GROUPS.find((g) => g.value === r.groupBy)?.column;
  return (
    <>
      <p
        className={label.stale ? "scope-badge stale" : "scope-badge"}
        data-testid="cloud-label"
        role={label.stale ? "alert" : undefined}
      >
        {t("All registers (cloud) · {status}", { status: label.text })}
      </p>
      <table
        className="sales-table"
        aria-label={tableName(r.groupBy, t)}
      >
        <thead>
          <tr>
            <th scope="col">{column ? t(column) : ""}</th>
            <th scope="col" className="num">
              {tender ? t("Taken") : t("Sales")}
            </th>
            {!tender && (
              <>
                <th scope="col" className="num">
                  {t("Discounts")}
                </th>
                <th scope="col" className="num">
                  {t("Tax")}
                </th>
              </>
            )}
            <th scope="col" className="num">
              {t("Refunds")}
            </th>
            <th scope="col" className="num">
              {t("Net")}
            </th>
          </tr>
        </thead>
        <tbody>
          {r.rows.map((x) => (
            <tr key={x.key} data-testid="cloud-row">
              <th scope="row">{name(x, r.groupBy, t)}</th>
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
              <th scope="row">{t("Total")}</th>
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
        <p className="small">{t("No sales from any register in this period.")}</p>
      )}
      {r.registers.length > 0 && (
        <details>
          <summary>{t("Registers")}</summary>
          <ul className="small" aria-label={t("Registers")}>
            {r.registers.map((g) => (
              <li key={g.code}>
                <strong>{g.code}</strong> {g.name} ·{" "}
                {g.current
                  ? t("up to date")
                  : g.pendingKnown
                    ? t("has data not yet sent")
                    : g.lastAckAt
                      ? t("last heard from {time}", { time: clock(g.lastAckAt) })
                      : t("not heard from yet")}
              </li>
            ))}
          </ul>
        </details>
      )}
      <p className="small">
        {t("Live sales only; refunds are of returns made in the period, at any register.")}
      </p>
    </>
  );
}
