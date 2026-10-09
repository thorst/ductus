// An operator acting through the UI against the demo engine (scripts/demo-ui.sh): correcting a
// quarantined message, resending from the quarantine screen, pausing and resuming a destination.
// Runs after ui.spec.ts (playwright.config.ts), whose counts these change.

import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page } from "@playwright/test";

const token = process.env.DUCTUS_OPERATOR_TOKEN ?? "";

async function signIn(page: Page, path: string) {
  await page.goto(path);
  await page.getByLabel("Token").fill(token);
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
}

const quarantined = (page: Page) => page.getByRole("table", { name: "Quarantined deliveries" }).locator("tbody tr");

test("an operator corrects a quarantined message: a copy goes, the original is replaced", async ({ page }) => {
  await signIn(page, "/quarantine");
  await expect(quarantined(page)).toHaveCount(4);
  await quarantined(page).first().getByRole("link").click();
  await expect(page.getByRole("heading", { level: 1 })).toContainText("quarantined");
  const original = page.url();

  await page.getByRole("button", { name: "Edit and resend" }).click();
  const dialog = page.getByRole("dialog", { name: /Edit and resend/ });
  await expect(dialog.getByLabel("default (quarantined)")).toBeChecked();
  const { violations } = await new AxeBuilder({ page }).include("[role=dialog]").analyze();
  expect(violations.map((v) => v.id)).toEqual([]);
  const editor = dialog.getByLabel(/Message/);
  await editor.fill((await editor.inputValue()).replace("100003^^^MRN", "100004^^^MRN"));
  await dialog.getByLabel(/Reason/).fill("patient merged into 100004");
  await dialog.getByRole("button", { name: "Queue the copy" }).click();

  // The copy: its own page, linked to the original, sent (the code accepts patient 100004).
  await expect(page.getByRole("note")).toContainText("An edited copy of message");
  await expect(page.getByRole("heading", { level: 1 })).toContainText("sent");
  await page.getByRole("button", { name: "Raw" }).click();
  await expect(page.locator("pre")).toContainText("PID|1||100004^^^MRN");

  // The original: as received, its delivery replaced, and the edit in its history.
  await page.goto(original);
  await expect(page.getByRole("heading", { level: 1 })).toContainText("filtered");
  await expect(page.getByRole("region", { name: "Destinations" })).toContainText("replaced by edited copy");
  const history = page.getByRole("region", { name: "History" });
  await expect(history).toContainText("Edited: copy");
  await expect(history).toContainText("patient merged into 100004");
  await page.screenshot({ path: "test-results/message-edited.png", fullPage: true });
});

test("resending from the quarantine screen", async ({ page }) => {
  await signIn(page, "/quarantine");
  await expect(quarantined(page)).toHaveCount(3);
  await quarantined(page).first().getByRole("checkbox").check();
  await page.getByRole("button", { name: "Resend 1 selected" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Resend" }).click();
  await expect(page.getByRole("status")).toContainText("Resent 1 delivery.");
  // The code still rejects patient 100003: quarantined again, with the resend in its history.
  await expect(quarantined(page)).toHaveCount(3);
  await quarantined(page).first().getByRole("link").click();
  await expect(page.getByRole("region", { name: "History" })).toContainText("Resent");

  await page.goto("/quarantine");
  await page.getByLabel("Select all listed").check();
  await page.getByRole("button", { name: "Resend 3 selected" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Resend" }).click();
  await expect(page.getByRole("status")).toContainText("Resent 3 deliveries.");
  await page.screenshot({ path: "test-results/quarantine.png", fullPage: true });
});

test("pausing a destination, then giving the reason, then resuming it", async ({ page }) => {
  await signIn(page, "/interfaces/adt");
  const archive = page.getByRole("region", { name: "archive" });
  await archive.getByRole("button", { name: "Pause archive" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Pause" }).click();
  await expect(archive).toContainText("Paused");
  await expect(archive).toContainText("no reason yet");

  await archive.getByRole("button", { name: "Add the reason" }).click();
  await page
    .getByRole("dialog")
    .getByLabel(/Reason/)
    .fill("archive disk being replaced");
  await page.getByRole("dialog").getByRole("button", { name: "Save the reason" }).click();
  await expect(archive).toContainText("archive disk being replaced");

  await archive.getByRole("button", { name: "Resume archive" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Resume" }).click();
  await expect(archive.getByRole("button", { name: "Pause archive" })).toBeVisible();
  await expect(archive).not.toContainText("Paused");
});
