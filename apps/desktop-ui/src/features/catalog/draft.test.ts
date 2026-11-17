import { describe, expect, it } from "vitest";
import { blankDraft, fromProduct, isProblem, regrid, toInput, variantLabel, type Axis, type VariantDraft } from "./draft";

/** The element at `i`, failing the test if it is missing. */
function at<T>(xs: T[], i = 0): T {
  const x = xs[i];
  if (x === undefined) throw new Error(`no element ${i}`);
  return x;
}

const axes = (spec: Record<string, string>): Axis[] => Object.entries(spec).map(([name, values]) => ({ name, values }));

describe("variant grid", () => {
  it("3 sizes × 2 colours make 6 variants in axis order", () => {
    const grid = regrid(axes({ size: "S, M, L", colour: "Red, Blue" }), []);
    expect(grid.map((v) => variantLabel(v, axes({ size: "", colour: "" })))).toEqual([
      "S / Red",
      "S / Blue",
      "M / Red",
      "M / Blue",
      "L / Red",
      "L / Blue",
    ]);
  });

  it("keeps what was typed for combinations that still exist; a simple product's data moves to the first", () => {
    const simple: VariantDraft = { ...at(blankDraft("t").variants), price: "9.99", sku: "BASE" };
    const first = regrid(axes({ size: "S, M" }), [simple]);
    expect(first.map((v) => [v.options.size, v.price, v.sku])).toEqual([
      ["S", "9.99", "BASE"],
      ["M", "9.99", ""],
    ]);
    at(first, 1).sku = "M-SKU";
    const second = regrid(axes({ size: "M, L" }), first);
    expect(second.map((v) => [v.options.size, v.sku])).toEqual([
      ["M", "M-SKU"],
      ["L", ""],
    ]);
  });

  it("ignores empty axes and duplicate values", () => {
    expect(regrid(axes({ size: "S, S, ,M", "": "x" }), [])).toHaveLength(2);
    expect(regrid([], [])).toHaveLength(1);
  });
});

describe("draft → IPC input", () => {
  it("scales prices and quantities as strings, never floats", () => {
    const d = blankDraft("tax-1", "5449000000996");
    d.name = " Cola ";
    at(d.variants, 0).price = "1.20";
    at(d.variants, 0).lowStock = "2.5";
    const input = toInput(d, 2);
    if (isProblem(input)) throw new Error(input.message);
    expect(input.name).toBe("Cola");
    expect(input.variants[0]).toMatchObject({
      priceMinor: "120",
      scanQtyMilli: "1000",
      lowStockThresholdMilli: "2500",
      trackStock: true,
      barcodes: [{ raw: "5449000000996", qualifier: "primary", packQtyMilli: "1000" }],
    });
    expect(toInput({ ...d, productType: "service" }, 2)).toMatchObject({ variants: [{ trackStock: false }] });
    // Zero-decimal currencies (JPY) take whole numbers only.
    at(d.variants, 0).price = "120";
    expect(toInput(d, 0)).toMatchObject({ variants: [{ priceMinor: "120" }] });
  });

  it("names the input at fault", () => {
    const d = blankDraft("t");
    expect(toInput(d, 2)).toEqual({ field: "name", message: "Enter the product name." });
    d.name = "X";
    at(d.variants, 0).price = "1.234";
    expect(toInput(d, 2)).toMatchObject({ field: "price", variant: 0 });
    at(d.variants, 0).price = "1";
    at(d.variants, 0).barcodes = [{ raw: "CASE", qualifier: "pack", packQty: "0" }];
    expect(toInput(d, 2)).toMatchObject({ field: "barcode", variant: 0 });
  });

  it("round-trips a stored product", () => {
    const d = fromProduct(
      {
        productId: "p",
        name: "Tee",
        description: null,
        categoryId: null,
        productType: "stock",
        unit: "ea",
        taxCategoryId: "t",
        quickSelect: true,
        archived: false,
        version: 3,
        variants: [
          { variantId: "v1", isDefault: true, sku: "T-S", options: { size: "S" }, priceMinor: "1500", costMinor: null, trackStock: true, lowStockThresholdMilli: null, scanQtyMilli: "1000", barcodes: [{ value: "x", raw: "CASE6", symbology: "code128", qualifier: "pack", packQtyMilli: "6000" }], archived: false },
          { variantId: "v2", isDefault: false, sku: "T-M", options: { size: "M" }, priceMinor: "1600", costMinor: null, trackStock: true, lowStockThresholdMilli: "5000", scanQtyMilli: "1000", barcodes: [], archived: false },
        ],
      },
      2,
    );
    expect(d.axes).toEqual([{ name: "size", values: "S, M" }]);
    expect(d.variants.map((v) => [v.price, v.lowStock, v.barcodes[0]?.packQty])).toEqual([
      ["15.00", "", "6"],
      ["16.00", "5", undefined],
    ]);
    const input = toInput(d, 2);
    if (isProblem(input)) throw new Error(input.message);
    expect(input.variants.map((v) => [v.variantId, v.priceMinor])).toEqual([
      ["v1", "1500"],
      ["v2", "1600"],
    ]);
  });
});
