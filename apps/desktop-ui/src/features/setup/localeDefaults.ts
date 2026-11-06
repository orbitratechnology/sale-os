/** Setup defaults from the operating system's locale (E04-T02 step 2). Always editable. */

const CURRENCY_BY_COUNTRY: Record<string, string> = {
  GB: "GBP", US: "USD", CA: "CAD", AU: "AUD", NZ: "NZD", IE: "EUR", DE: "EUR", FR: "EUR", ES: "EUR",
  IT: "EUR", NL: "EUR", BE: "EUR", AT: "EUR", PT: "EUR", FI: "EUR", NG: "NGN", GH: "GHS", KE: "KES",
  ZA: "ZAR", IN: "INR", AE: "AED", SA: "SAR", JP: "JPY", KW: "KWD", SG: "SGD", MY: "MYR", PH: "PHP",
};

export function localeDefaults(): { countryCode: string; timezone: string; currencyCode: string } {
  let countryCode = "";
  try {
    countryCode = new Intl.Locale(navigator.language).maximize().region ?? "";
  } catch {
    // An unparseable locale leaves the field for the owner to fill in.
  }
  let timezone = "";
  try {
    timezone = Intl.DateTimeFormat().resolvedOptions().timeZone;
  } catch {
    // As above.
  }
  return { countryCode, timezone, currencyCode: CURRENCY_BY_COUNTRY[countryCode] ?? "" };
}
