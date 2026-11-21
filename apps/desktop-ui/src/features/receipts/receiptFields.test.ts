import { describe, expect, it } from "vitest";
import { receiptFields } from "./receiptFields";

describe("receiptFields", () => {
  it("leaves lines without right-to-left text alone", () => {
    expect(receiptFields("Tea                 2.50")).toBeNull();
    expect(receiptFields("සීනි 1kg")).toBeNull();
  });

  it("splits a right-to-left line into fields at wide gaps, keeping every space", () => {
    const line = "  خصم الصيف          -1.00";
    const parts = receiptFields(line);
    expect(parts).toEqual([
      { text: "  ", field: false },
      { text: "خصم الصيف", field: true },
      { text: "          ", field: false },
      { text: "-1.00", field: true },
    ]);
    expect(parts?.map((p) => p.text).join("")).toBe(line);
    // A single space stays inside its field.
    expect(receiptFields("عصير Rani 250 مل")).toEqual([{ text: "عصير Rani 250 مل", field: true }]);
  });
});
