import { defineConfig } from "vitest/config";

// Browser-driven tests (Playwright + Chromium). Run with `pnpm test:e2e`.
export default defineConfig({
  test: { include: ["e2e/**/*.e2e.ts"], environment: "node", testTimeout: 60_000, fileParallelism: false },
});
