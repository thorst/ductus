import { defineConfig, devices } from "@playwright/test";

// The v0.5 story (scripts/demo-v05.sh starts the engine, the directory, the mail sink and a
// certificate server; the story itself starts and stops the lab's receiver). One night of
// operations in order, so one worker and the steps in sequence.
export default defineConfig({
  testDir: "story",
  outputDir: "test-results/story",
  fullyParallel: false,
  workers: 1,
  retries: 0,
  timeout: 60_000,
  reporter: [["line"], ["html", { open: "never", outputFolder: "playwright-report/story" }]],
  use: {
    baseURL: process.env.DUCTUS_URL ?? "http://127.0.0.1:7799",
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
    ...devices["Desktop Chrome"],
    viewport: { width: 1360, height: 900 },
  },
});
