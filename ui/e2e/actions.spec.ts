// An operator acting through the UI against the demo engine (scripts/demo-ui.sh): correcting a
// quarantined message, resending from the quarantine screen, pausing and resuming a destination,
// confirming a shorter retention, connection checks, draining; and an admin managing tokens.
// Runs after ui.spec.ts (playwright.config.ts), whose counts these change.

import { readFileSync } from "node:fs";
import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page } from "@playwright/test";

const token = process.env.DUCTUS_OPERATOR_TOKEN ?? "";
const admin = process.env.DUCTUS_ADMIN_TOKEN ?? "";

async function signIn(page: Page, path: string, as = token) {
  await page.goto(path);
  await page.getByRole("button", { name: "Use an API token instead" }).click();
  await page.getByLabel("Token").fill(as);
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page).not.toHaveURL(/\/sign-in/); // the sign-in page has an h1 too
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

test("confirming a shorter retention: the interface's name is typed first", async ({ page }) => {
  await signIn(page, "/retention");
  const rows = page.getByRole("table", { name: "Retention by interface" });
  await rows.getByRole("button", { name: "Confirm billing's shorter retention" }).click();
  const dialog = page.getByRole("dialog", { name: "Shorten billing's retention to 7 days" });
  await expect(dialog).toContainText("can't be undone");
  const { violations } = await new AxeBuilder({ page }).include("[role=dialog]").analyze();
  expect(violations.map((v) => v.id)).toEqual([]);
  const confirm = dialog.getByRole("button", { name: "Delete and shorten" });
  await expect(confirm).toBeDisabled();
  await dialog.getByLabel(/Type billing to confirm/).fill("billin");
  await expect(confirm).toBeDisabled();
  await dialog.getByLabel(/Type billing to confirm/).fill("billing");
  await confirm.click();
  // Applied: new messages keep 1 week; the older ones move as the change runs (billing's wait
  // for their paused destination, so the change stays in progress).
  const billing = rows.getByRole("row", { name: /billing/ });
  await expect(billing).toContainText("1w (7 days)");
  await expect(billing).toContainText("Changing from 49 to 7 days");
  await expect(rows.getByRole("button", { name: /Confirm/ })).toHaveCount(0);
});

test("running the connection checks and saving the report", async ({ page }) => {
  await signIn(page, "/connections");
  await page.getByLabel("Wait per step").selectOption("2000");
  await page.getByRole("button", { name: "Run the checks" }).click();
  const report = page.getByRole("region", { name: "Report" });
  await expect(report.getByRole("status")).toContainText(/\d+ ok, \d+ warnings?, [1-9]\d* failed/, { timeout: 20_000 });
  // Nobody listens for adt's lab: its check fails first, with what to do.
  const first = report.getByRole("table", { name: "Checks" }).locator("tbody tr").first();
  await expect(first).toContainText("Failed");
  await expect(first).toContainText("127.0.0.1:6799");
  await page.screenshot({ path: "test-results/connections.png", fullPage: true });
  const [download] = await Promise.all([
    page.waitForEvent("download"),
    report.getByRole("button", { name: "Save the report" }).click(),
  ]);
  const saved = JSON.parse(readFileSync((await download.path())!, "utf8")) as { checks: unknown[] };
  expect(saved.checks.length).toBeGreaterThan(1);
});

test("draining, asking it to stop when drained, then taking messages again", async ({ page }) => {
  await signIn(page, "/drain");
  await page.getByRole("button", { name: "Start draining" }).click();
  const start = page.getByRole("dialog", { name: "Start draining" });
  await expect(start.getByLabel(/Stop the engine when drained/)).not.toBeChecked();
  await start.getByRole("button", { name: "Drain", exact: true }).click();
  // adt's lab is down, so the drain can't finish: it says what it waits for.
  const state = page.getByRole("region", { name: "Drain state" });
  await expect(state.getByRole("status")).toContainText("Draining");
  await expect(state).toContainText("Still delivering for: adt");
  await expect(page.getByRole("banner")).toContainText("Draining");
  // The header's badge and the page only look like this during a drain: axe them now.
  for (const t of ["light", "dark", "contrast"]) {
    await page.evaluate((theme) => localStorage.setItem("ductus.theme", theme), t);
    await page.reload();
    await expect(state.getByRole("status")).toContainText("Draining");
    const tags = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", ...(t === "contrast" ? ["wcag2aaa"] : [])];
    const { violations } = await new AxeBuilder({ page }).withTags(tags).analyze();
    expect(
      violations.map((v) => `${v.id}: ${v.nodes.map((n) => n.target.join(" ")).join(", ")}`),
      t,
    ).toEqual([]);
  }
  await page.evaluate(() => localStorage.removeItem("ductus.theme"));
  await page.reload();

  await state.getByRole("button", { name: "Stop when drained" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Stop when drained" }).click();
  await expect(state.getByRole("status")).toContainText("The engine stops once drained.");
  await page.screenshot({ path: "test-results/drain.png", fullPage: true });

  await state.getByRole("button", { name: "Take messages again" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Take messages" }).click();
  await expect(state.getByRole("status")).toContainText("Taking messages.");
  await expect(page.getByRole("banner")).not.toContainText("Draining");
});

test("an admin creates a token (its secret works, shown once), then revokes it", async ({ page, request }) => {
  await signIn(page, "/", admin);
  await page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: "Tokens" }).click();
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Tokens");
  for (const t of ["light", "dark", "contrast"]) {
    await page.evaluate((theme) => localStorage.setItem("ductus.theme", theme), t);
    await page.reload();
    await expect(page.getByRole("table", { name: "Tokens" })).toBeVisible();
    const tags = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", ...(t === "contrast" ? ["wcag2aaa"] : [])];
    const { violations } = await new AxeBuilder({ page }).withTags(tags).analyze();
    expect(
      violations.map((v) => v.id),
      `${t} theme`,
    ).toEqual([]);
  }

  const name = `e2e-script-${Date.now()}`;
  await page.getByRole("button", { name: "New token" }).click();
  const create = page.getByRole("dialog", { name: "New token" });
  await create.getByLabel("Name").fill(name);
  await create.getByLabel(/^operator/).check();
  await create.getByRole("button", { name: "Create" }).click();

  const shown = page.getByRole("dialog", { name: `${name} (operator)` });
  await expect(shown).toContainText("shown only this once");
  const secret = await shown.getByLabel("Token").inputValue();
  expect(secret).toMatch(/^dux_[0-9a-f]{64}$/);
  const { violations } = await new AxeBuilder({ page }).include("[role=dialog]").analyze();
  expect(violations.map((v) => v.id)).toEqual([]);
  const asScript = { headers: { Authorization: `Bearer ${secret}` } };
  const me = await request.get("/api/v1/session", asScript);
  expect(await me.json()).toMatchObject({ name, role: "operator" });
  await shown.getByRole("button", { name: "Done" }).click();

  const row = page.getByRole("table", { name: "Tokens" }).getByRole("row", { name: new RegExp(name) });
  await expect(row).toContainText("operator");
  await row.getByRole("button", { name: `Revoke ${name}` }).click();
  await page
    .getByRole("dialog", { name: `Revoke ${name}` })
    .getByRole("button", { name: "Revoke" })
    .click();
  await expect(row).toHaveCount(0);
  expect((await request.get("/api/v1/session", asScript)).status()).toBe(401);
});
