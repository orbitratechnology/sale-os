import { describe, expect, it } from "vitest";
import { translate } from "./index";
import { si } from "./si";
import { ta } from "./ta";
import registry from "../../../../crates/pos-application/src/settings.rs?raw";

/**
 * The till in Sinhala and Tamil (D-02): every English sentence the screens show through
 * `t("…")`, the tables of copy they translate (error, approval and printer messages, tender
 * names) and the settings registry's labels has a Sinhala and a Tamil entry with exactly the
 * same `{placeholders}`.
 */
const sources = import.meta.glob<string>(
  ["../**/*.{ts,tsx}", "!../i18n/**", "!../ipc/**", "!**/*.test.ts", "!../main.tsx", "!../features/scan/harness.tsx"],
  { query: "?raw", import: "default", eager: true },
);

const unescape = (s: string) => s.replace(/\\(.)/g, "$1");

/**
 * The settings registry lives in Rust: the screen shows each setting's `label` and `help`
 * through `t`, so they need translations too.
 */
function registryKeys(): string[] {
  return [...registry.matchAll(/^\s+(?:label|help): "((?:[^"\\]|\\.)*)",$/gm)].map((m) => unescape(m[1] ?? ""));
}

/**
 * `t("…")` literals, and the string values of the copy tables passed through `t`: any
 * `const COPY`, `const WHAT`, `const FAILURE` or `const …_COPY`, up to its closing `};` / `];`.
 */
function englishKeys(): Set<string> {
  const keys = new Set<string>();
  for (const [file, text] of Object.entries(sources)) {
    for (const m of text.matchAll(/\bt\(\s*"((?:[^"\\]|\\.)*)"/g)) keys.add(unescape(m[1] ?? ""));
    for (const table of text.matchAll(/^(?:export )?const (?:\w+_)?(?:COPY|WHAT|FAILURE)\b/gm)) {
      const rest = text.slice(table.index);
      const end = rest.search(/^[}\]];/m);
      const block = rest.slice(0, end < 0 ? undefined : end);
      for (const m of block.matchAll(/:\s*"((?:[^"\\]|\\.)*)"/g)) keys.add(unescape(m[1] ?? ""));
    }
    if (file.endsWith("PaymentPanel.tsx")) {
      for (const m of text.matchAll(/label: "([^"]+)"/g)) keys.add(m[1] ?? "");
      keys.add("Cash");
    }
  }
  for (const k of registryKeys()) keys.add(k);
  keys.add("this action");
  return keys;
}

const placeholders = (s: string) => [...s.matchAll(/\{(\w+)\}/g)].map((m) => m[1]).sort();

describe("translate", () => {
  it("fills placeholders and falls back to the English", () => {
    expect(translate("en", "Not {name}?", { name: "Amal" })).toBe("Not Amal?");
    expect(translate("si", "a sentence nobody translated")).toBe("a sentence nobody translated");
    expect(translate("ta", "Parked ({n})", { n: 3 })).toContain("3");
    expect(translate("si", "Parked ({n})", { n: 3 })).not.toBe("Parked (3)");
    // A context tells two meanings of one English word apart, and is never shown.
    expect(translate("en", "verb::Change")).toBe("Change");
    expect(translate("si", "verb::Change")).not.toBe(translate("si", "Change"));
    expect(translate("ta", "stock quantity::Change")).not.toBe(translate("ta", "Change"));
  });
});

describe.each([
  ["Sinhala", si],
  ["Tamil", ta],
] as const)("%s catalogue", (_name, catalog) => {
  const keys = englishKeys();

  it("covers every sentence the cashier screens show", () => {
    expect(keys.size).toBeGreaterThan(250);
    const missing = [...keys].filter((k) => !catalog[k]?.trim());
    expect(missing).toEqual([]);
  });

  it("keeps each entry's placeholders exactly", () => {
    const wrong = Object.entries(catalog).filter(([en, tr]) => placeholders(en).join() !== placeholders(tr).join());
    expect(wrong).toEqual([]);
  });

  it("has no stale entries", () => {
    expect(Object.keys(catalog).filter((k) => !keys.has(k))).toEqual([]);
  });

  it("keeps key names, digits and product terms", () => {
    for (const [en, tr] of Object.entries(catalog)) {
      for (const token of ["F2", "F5", "F6", "F10", "⇧F5", "Sale OS", "SKU", "PDF"]) {
        if (en.includes(token)) expect(tr, en).toContain(token);
      }
      for (const n of en.match(/\d+(\.\d+)?/g) ?? []) expect(tr, en).toContain(n);
    }
  });
});
