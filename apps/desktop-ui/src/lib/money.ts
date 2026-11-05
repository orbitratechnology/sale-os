/**
 * Money display helpers (ADR-001).
 *
 * Amounts arrive from Rust as decimal strings of minor units, e.g. "123456" for 1,234.56 USD.
 * This module formats them with pure string operations: no JS `number` is ever involved,
 * so amounts above 2^53 stay exact and no binary-float rounding can occur.
 * The UI never computes totals; Rust does (ADR-018).
 */

export interface MoneyFormat {
  /** ISO-4217 minor-unit exponent, 0..4. */
  exponent: number;
  /** Decimal separator, default ".". */
  decimal?: string;
  /** Group separator, default ","; `null` for none. */
  group?: string | null;
}

const MINOR_UNITS = /^-?\d+$/;

/** Formats a minor-unit string: formatMoney("123456", { exponent: 2 }) === "1,234.56". */
export function formatMoney(minor: string, format: MoneyFormat): string {
  if (!MINOR_UNITS.test(minor)) {
    throw new TypeError(`not a minor-unit integer string: ${JSON.stringify(minor)}`);
  }
  const { exponent } = format;
  if (!Number.isInteger(exponent) || exponent < 0 || exponent > 4) {
    throw new RangeError(`currency exponent must be 0..4, got ${exponent}`);
  }
  const decimal = format.decimal ?? ".";
  const group = format.group === undefined ? "," : format.group;

  const negative = minor.startsWith("-");
  const digits = (negative ? minor.slice(1) : minor).replace(/^0+(?=\d)/, "");
  const padded = digits.padStart(exponent + 1, "0");
  const intPart = padded.slice(0, padded.length - exponent);
  const fracPart = padded.slice(padded.length - exponent);

  const grouped = group === null ? intPart : intPart.replace(/\B(?=(\d{3})+(?!\d))/g, group);
  const isZero = /^0+$/.test(digits);
  const sign = negative && !isZero ? "-" : "";
  return exponent > 0 ? `${sign}${grouped}${decimal}${fracPart}` : `${sign}${grouped}`;
}
