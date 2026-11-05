import js from "@eslint/js";
import tseslint from "typescript-eslint";
import reactHooks from "eslint-plugin-react-hooks";
import globals from "globals";

// Money and quantities cross IPC as decimal strings of minor/milli units (ADR-001/002).
// UI code formats them; it never turns them into JS numbers.
const noFloatMoney = {
  "no-restricted-globals": [
    "error",
    { name: "parseFloat", message: "Money/quantities are strings of minor units; use lib/money.ts (ADR-001)." },
  ],
  "no-restricted-syntax": [
    "error",
    {
      selector: "CallExpression[callee.name='Number']",
      message: "Do not convert money/quantities to JS numbers; use lib/money.ts (ADR-001).",
    },
    {
      selector: "MemberExpression[object.name='Number'][property.name='parseFloat']",
      message: "Do not convert money/quantities to JS numbers; use lib/money.ts (ADR-001).",
    },
  ],
};

export default tseslint.config(
  { ignores: ["dist", "src/ipc/bindings.ts"] },
  js.configs.recommended,
  ...tseslint.configs.strict,
  {
    files: ["**/*.{ts,tsx}"],
    languageOptions: { globals: globals.browser },
    plugins: { "react-hooks": reactHooks },
    rules: {
      ...reactHooks.configs.recommended.rules,
    },
  },
  {
    files: ["src/features/**/*.{ts,tsx}", "src/lib/**/*.ts"],
    ignores: ["**/*.test.ts"],
    rules: noFloatMoney,
  },
);
