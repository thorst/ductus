import { defineConfig, devices } from "@playwright/test";

// End-to-end tests run against a real engine started by scripts/demo-ui.sh (DUCTUS_URL), so
// they exercise the embedded UI, the session cookie and the API together.
export default defineConfig({
  testDir: "e2e",
  outputDir: "test-results",
  fullyParallel: false,
  workers: 1,
  retries: 0,
  timeout: 30_000,
  reporter: [["line"], ["html", { open: "never" }]],
  use: {
    baseURL: process.env.DUCTUS_URL ?? "http://127.0.0.1:7700",
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
  },
  // The read-only tests first (they check the demo's counts), then the ones that act and change them.
  projects: [
    {
      name: "read",
      testMatch: "ui.spec.ts",
      use: { ...devices["Desktop Chrome"], viewport: { width: 1360, height: 900 } },
    },
    {
      name: "act",
      testMatch: "actions.spec.ts",
      dependencies: ["read"],
      use: { ...devices["Desktop Chrome"], viewport: { width: 1360, height: 900 } },
    },
    {
      // People with passwords; makes users, so it goes last.
      name: "people",
      testMatch: "people.spec.ts",
      dependencies: ["act"],
      use: { ...devices["Desktop Chrome"], viewport: { width: 1360, height: 900 } },
    },
    {
      // The audit log and dismissing quarantined messages: dismisses a delivery, so it goes after
      // the tests that count quarantines.
      name: "audit",
      testMatch: "audit.spec.ts",
      dependencies: ["people"],
      use: { ...devices["Desktop Chrome"], viewport: { width: 1360, height: 900 } },
    },
    {
      // Alerts: acknowledges and silences, so it goes after the tests that read health and counts.
      name: "alerts",
      testMatch: "alerts.spec.ts",
      dependencies: ["audit"],
      use: { ...devices["Desktop Chrome"], viewport: { width: 1360, height: 900 } },
    },
  ],
});
