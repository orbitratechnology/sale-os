/**
 * The product editor's working copy (E06-T06b) and its conversion to and from the IPC
 * shapes. Pure: no React, no IPC, so it is unit-tested directly.
 *
 * Prices and quantities stay strings end to end (ADR-001/002): typed text is scaled to
 * minor units or milli-units by string manipulation, and Rust validates again.
 */
import type { BarcodeQualifier, ProductDto_Serialize, ProductInputDto_Deserialize, ProductType, Unit } from "../../ipc/bindings";
import { formatMilli, scaleDecimal } from "../../lib/decimal";
import { formatMoney } from "../../lib/money";

export interface Axis {
  name: string;
  /** Comma-separated as typed, e.g. "S, M, L". */
  values: string;
}

export interface BarcodeDraft {
  raw: string;
  qualifier: BarcodeQualifier;
  /** Units a pack barcode adds, as typed ("12"). */
  packQty: string;
}

export interface VariantDraft {
  variantId: string | null;
  options: Record<string, string>;
  price: string;
  sku: string;
  barcodes: BarcodeDraft[];
  lowStock: string;
  scanQty: string;
}

export interface ProductDraft {
  productId: string | null;
  version: number | null;
  name: string;
  description: string;
  productType: ProductType;
  unit: Unit;
  taxCategoryId: string;
  quickSelect: boolean;
  axes: Axis[];
  variants: VariantDraft[];
}

/** A problem found before sending: which input, and what to say. */
export interface FieldProblem {
  field: string;
  message: string;
  /** For per-variant inputs: the variant's index. */
  variant?: number;
}

export function blankVariant(options: Record<string, string> = {}): VariantDraft {
  return { variantId: null, options, price: "", sku: "", barcodes: [], lowStock: "", scanQty: "1" };
}

export function blankDraft(taxCategoryId: string, barcode?: string): ProductDraft {
  const v = blankVariant();
  if (barcode) v.barcodes.push({ raw: barcode, qualifier: "primary", packQty: "1" });
  return {
    productId: null,
    version: null,
    name: "",
    description: "",
    productType: "stock",
    unit: "ea",
    taxCategoryId,
    quickSelect: false,
    axes: [],
    variants: [v],
  };
}

export function axisValues(a: Axis): string[] {
  return [...new Set(a.values.split(",").map((v) => v.trim()).filter(Boolean))];
}

/** Options identity, independent of key order. */
export function optionsKey(o: Record<string, string>): string {
  return JSON.stringify(Object.keys(o).sort().map((k) => [k, o[k]]));
}

/**
 * The variant grid for the axes: every combination, in axis order. Variants that already
 * exist for a combination keep their prices, SKUs and barcodes. With no axes there is one
 * variant: the first existing one.
 */
export function regrid(axes: Axis[], existing: VariantDraft[]): VariantDraft[] {
  const usable = axes.map((a) => ({ name: a.name.trim(), values: axisValues(a) })).filter((a) => a.name && a.values.length);
  if (usable.length === 0) return [{ ...(existing[0] ?? blankVariant()), options: {} }];
  let combos: Record<string, string>[] = [{}];
  for (const a of usable) combos = combos.flatMap((c) => a.values.map((v) => ({ ...c, [a.name]: v })));
  const byKey = new Map(existing.map((v) => [optionsKey(v.options), v]));
  // Turning a simple product into a variant product: its first variant keeps its data.
  const simple = existing.length === 1 && Object.keys(existing[0]?.options ?? {}).length === 0 ? existing[0] : undefined;
  return combos.map((o, i) => {
    const kept = byKey.get(optionsKey(o)) ?? (i === 0 ? simple : undefined);
    return kept ? { ...kept, options: o } : { ...blankVariant(o), price: existing[0]?.price ?? "" };
  });
}

export function variantLabel(v: VariantDraft, axes: Axis[]): string {
  return axes.map((a) => v.options[a.name.trim()]).filter(Boolean).join(" / ");
}

export function fromProduct(p: ProductDto_Serialize, exponent: number): ProductDraft {
  const names: string[] = [];
  for (const v of p.variants) for (const k of Object.keys(v.options)) if (!names.includes(k)) names.push(k);
  const axes = names.map((name) => ({
    name,
    values: [...new Set(p.variants.map((v) => v.options[name]).filter((x): x is string => !!x))].join(", "),
  }));
  return {
    productId: p.productId,
    version: p.version,
    name: p.name,
    description: p.description ?? "",
    productType: p.productType,
    unit: p.unit,
    taxCategoryId: p.taxCategoryId,
    quickSelect: p.quickSelect,
    axes,
    variants: p.variants
      .filter((v) => !v.archived)
      .map((v) => ({
        variantId: v.variantId,
        options: v.options,
        price: formatMoney(v.priceMinor, { exponent, group: null }),
        sku: v.sku ?? "",
        barcodes: v.barcodes.map((b) => ({ raw: b.raw, qualifier: b.qualifier, packQty: formatMilli(b.packQtyMilli) })),
        lowStock: v.lowStockThresholdMilli === null ? "" : formatMilli(v.lowStockThresholdMilli),
        scanQty: formatMilli(v.scanQtyMilli),
      })),
  };
}

/** Builds the IPC input, or the first problem a person must fix. */
export function toInput(d: ProductDraft, exponent: number): ProductInputDto_Deserialize | FieldProblem {
  if (!d.name.trim()) return { field: "name", message: "Enter the product name." };
  if (d.variants.length === 0) return { field: "variants", message: "Add at least one variant." };
  const tracked = d.productType === "stock";
  const variants = [];
  for (const [i, v] of d.variants.entries()) {
    const price = scaleDecimal(v.price, exponent);
    if (price === null) return { field: "price", variant: i, message: "Enter a price like 1.20." };
    const scanQty = scaleDecimal(v.scanQty || "1", 3);
    if (scanQty === null || scanQty === "0") return { field: "scanQty", variant: i, message: "Enter how many one scan adds, e.g. 1." };
    const lowStock = v.lowStock.trim() ? scaleDecimal(v.lowStock, 3) : null;
    if (v.lowStock.trim() && lowStock === null) return { field: "lowStockThreshold", variant: i, message: "Enter a number, e.g. 5." };
    const barcodes = [];
    for (const b of v.barcodes) {
      const pack = b.qualifier === "pack" ? scaleDecimal(b.packQty, 3) : "1000";
      if (pack === null || pack === "0") return { field: "barcode", variant: i, message: `Enter how many units barcode ${b.raw} adds.` };
      barcodes.push({ raw: b.raw, symbology: null, qualifier: b.qualifier, packQtyMilli: pack });
    }
    variants.push({
      variantId: v.variantId,
      sku: v.sku.trim() || null,
      options: v.options,
      priceMinor: price,
      costMinor: null,
      trackStock: tracked,
      lowStockThresholdMilli: lowStock,
      scanQtyMilli: scanQty,
      barcodes,
    });
  }
  return {
    name: d.name.trim(),
    description: d.description.trim() || null,
    categoryId: null,
    productType: d.productType,
    unit: d.unit,
    taxCategoryId: d.taxCategoryId,
    quickSelect: d.quickSelect,
    variants,
  };
}

export function isProblem(x: ProductInputDto_Deserialize | FieldProblem): x is FieldProblem {
  return "field" in x && "message" in x;
}
