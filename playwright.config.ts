import { defineConfig, devices } from "@playwright/test";

/**
 * Visual-fidelity harness config for tests/visual/visual-check.spec.ts.
 *
 * The spec navigates to "/?fixture=..." so a baseURL and a running dev server
 * are both required. Screenshot baselines are exported from Figma by hand —
 * see the header comment in the spec.
 */
export default defineConfig({
  testDir: "./tests",
  snapshotPathTemplate: "{testDir}/visual/baseline/{arg}{ext}",
  fullyParallel: true,
  reporter: [["list"]],
  use: {
    baseURL: "http://localhost:5173",
    // Fractional-DPI hairline checks override this per-context.
    deviceScaleFactor: 1,
    colorScheme: "dark",
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
  webServer: {
    command: "npm run dev",
    url: "http://localhost:5173",
    reuseExistingServer: true,
    timeout: 60_000,
  },
});
