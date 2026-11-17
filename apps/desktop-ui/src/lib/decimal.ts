/**
 * Decimal input and display for quantities and rates (ADR-001, ADR-002).
 *
 * What a person types ("1.5", "12,5") becomes a scaled integer *string* ("1500" milli-units)
 * by string manipulation only, so no binary floating point is ever involved. Rust validates
 * the result again (fractional "ea" quantities, discount bounds...).
 */

const DECIMAL_INPUT = /^\d+([.,]\d*)?$|^[.,]\d+$/;

/**
 * Scales a non-negative decimal by 10^scale: scaleDecimal("1.5", 3) === "1500".
 * Returns null for anything else: signs, letters, more fraction digits than `scale`.
 */
export function scaleDecimal(input: string, scale: number): string | null {
  const text = input.trim();
  if (!DECIMAL_INPUT.test(text)) return null;
  const [whole = "", frac = ""] = text.split(/[.,]/);
  if (frac.length > scale) return null;
  const digits = `${whole}${frac.padEnd(scale, "0")}`.replace(/^0+(?=\d)/, "");
  return digits === "" ? "0" : digits;
}

/** "12.5" (percent) to 125000 ppm. Null if invalid or above 100%. */
export function percentToPpm(input: string): number | null {
  const scaled = scaleDecimal(input, 4);
  if (scaled === null || scaled.length > 7) return null;
  const ppm = parseInt(scaled, 10);
  return ppm <= 1_000_000 ? ppm : null;
}

/** 125000 ppm to "12.5". */
export function ppmToPercent(ppm: number): string {
  return trimFraction(insertPoint(String(ppm), 4));
}

/** Milli-units to a display string: "1500" → "1.5", "3000" → "3". */
export function formatMilli(milli: string): string {
  if (!/^-?\d+$/.test(milli)) throw new TypeError(`not a milli-unit integer string: ${JSON.stringify(milli)}`);
  const negative = milli.startsWith("-");
  const out = trimFraction(insertPoint(negative ? milli.slice(1) : milli, 3));
  return negative && out !== "0" ? `-${out}` : out;
}

/** Adds `delta` whole units to a milli string (for the +/- keys). Never below 1 unit. */
export function stepMilli(milli: string, delta: 1 | -1): string {
  const next = BigInt(milli) + BigInt(delta * 1000);
  return next < 1000n ? "1000" : next.toString();
}

function insertPoint(digits: string, scale: number): string {
  const padded = digits.replace(/^0+(?=\d)/, "").padStart(scale + 1, "0");
  return `${padded.slice(0, padded.length - scale)}.${padded.slice(padded.length - scale)}`;
}

function trimFraction(s: string): string {
  return s.replace(/\.?0+$/, "") || "0";
}
