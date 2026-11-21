/** Right-to-left letters: Hebrew, Arabic, Syriac, Thaana, N'Ko and their presentation forms. */
const RTL = /[\u0590-\u08FF\uFB1D-\uFDFF\uFE70-\uFEFF]/;

export interface ReceiptPart {
  text: string;
  /** Text of a field (a name, label or amount); otherwise the spaces between fields. */
  field: boolean;
}

/**
 * A receipt line with right-to-left text, split into fields: text between runs of two or more
 * spaces, as the layout pads between a label and its amount. The preview puts each field in
 * its own `<bdi>`, so the browser orders each one by its first strong letter and an amount
 * stays in its column, as the PDF and the printer do (`pos_receipts::bidi`). `null` for a
 * line without right-to-left text: it is shown as it is.
 */
export function receiptFields(text: string): ReceiptPart[] | null {
  if (!RTL.test(text)) return null;
  return text
    .split(/( {2,})/)
    .map((part, i) => ({ text: part, field: i % 2 === 0 }))
    .filter((p) => p.text !== "");
}
