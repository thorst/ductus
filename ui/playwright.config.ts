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
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"], viewport: { width: 1360, height: 900 } } }],
});
