// Alerts (scripts/demo-ui.sh sets them up): orders' quarantine and the lab's stall are open, the
// header says so on every page, an operator acknowledges one and silences another (and ends the
// silence), tests a channel that works and one that can't be reached, and a viewer only looks.
// Every alert screen passes axe in every theme (AAA in High contrast) and fits a phone. Runs last
// (playwright.config.ts).

import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Browser, type Page } from "@playwright/test";

function person(key: string) {
  const [user = "", ...rest] = (process.env[key] ?? "").split(":");
  return { user, password: rest.join(":") };
}

const operator = person("DUCTUS_PERSON_OPERATOR");
const viewer = person("DUCTUS_PERSON_VIEWER");

async function signInAs(page: Page, who: { user: string; password: string }, path = "/") {
  await page.goto(path);
  await expect(page).toHaveURL(/\/sign-in/);
  await page.getByLabel("Username").fill(who.user);
  await page.getByLabel("Password").fill(who.password);
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page).not.toHaveURL(/\/sign-in/);
  await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
}

async function elsewhere(browser: Browser, viewport = { width: 1360, height: 900 }) {
  const context = await browser.newContext({ viewport });
  return { context, page: await context.newPage() };
}

const AA_TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"];

test("the header and the overview say what is open, and the Alerts page lists it", async ({ page }) => {
  await signInAs(page, operator);
  const badge = page.getByRole("link", { name: /open alerts?/ });
  await expect(badge).toBeVisible({ timeout: 30_000 });
  await expect(badge).toContainText("critical");
  const card = page.getByRole("region", { name: "Open alerts" });
  await expect(card).toContainText("orders");
  await badge.click();
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Alerts");
  const open = page.getByRole("region", { name: "Open alerts" });
  const orders = open.getByRole("listitem").filter({ has: page.getByRole("link", { name: "orders", exact: true }) });
  await expect(orders).toContainText("Critical");
  await expect(orders).toContainText("error");
  await expect(orders).toContainText("quarantined");
  const lab = open.getByRole("listitem").filter({ has: page.getByRole("link", { name: "adt → lab" }) });
  await expect(lab).toContainText("stalled");
  // Billing is paused on purpose: a pause never alerts.
  await expect(open).not.toContainText("billing");
  await expect(page.getByRole("table", { name: "Alert history" })).toContainText("Fired");
});

test("an operator acknowledges one alert and silences another, then ends the silence", async ({ page }) => {
  await signInAs(page, operator, "/alerts");
  const open = page.getByRole("region", { name: "Open alerts" });
  await open.getByRole("button", { name: "Acknowledge the alert of orders" }).click();
  const orders = open.getByRole("listitem").filter({ has: page.getByRole("link", { name: "orders", exact: true }) });
  await expect(orders).toContainText(`acknowledged by ${operator.user}`);
  await expect(open.getByRole("button", { name: "Acknowledge the alert of orders" })).toHaveCount(0);

  await open.getByRole("button", { name: "Silence… adt → lab" }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog).toContainText("Messages still wait");
  await dialog.getByLabel("For").selectOption("1h");
  await dialog.getByLabel("Reason (optional, kept with the action)").fill("lab vendor maintenance");
  await page.screenshot({ path: "test-results/alerts-silence-dialog.png" });
  await dialog.getByRole("button", { name: "Silence", exact: true }).click();
  const lab = open.getByRole("listitem").filter({ has: page.getByRole("link", { name: "adt → lab" }) });
  await expect(lab).toContainText(`silenced by ${operator.user}`);
  await expect(lab).toContainText("lab vendor maintenance");
  await page.screenshot({ path: "test-results/alerts.png", fullPage: true });
  await lab.getByRole("button", { name: "End silence of adt → lab" }).click();
  await expect(lab).not.toContainText("silenced by");

  const history = page.getByRole("table", { name: "Alert history" });
  await expect(history).toContainText("Acknowledged");
  await expect(history).toContainText("Silenced");
  await expect(history).toContainText("Silence ended");
  await expect(history).toContainText(operator.user);

  // The interface page shows its own alerts, with the same controls.
  await page.goto("/interfaces/adt");
  const mine = page.getByRole("region", { name: "Alerts" });
  await expect(mine).toContainText("adt → lab");
  await expect(mine).toContainText("stalled");
  await expect(mine.getByRole("button", { name: "Silence… adt → archive" })).toBeVisible();
});

test("a channel is tested: one works, one says why it can't", async ({ page }) => {
  await signInAs(page, operator, "/alerts");
  const channels = page.getByRole("region", { name: "Channels" });
  const table = channels.getByRole("table", { name: "Channels" });
  const chat = table.locator("tbody tr").filter({ hasText: "chat" });
  await expect(chat).toContainText("http://127.0.0.1/…");
  await chat.getByRole("button", { name: "Send test to chat" }).click();
  await expect(chat.getByRole("status")).toHaveText("Sent");
  const pager = table.locator("tbody tr").filter({ hasText: "pager" });
  await pager.getByRole("button", { name: "Send test to pager" }).click();
  await expect(pager.getByRole("status")).toContainText("calling http://127.0.0.1/…");
  // Its URL's path (often a webhook's secret) is never shown.
  await expect(channels).not.toContainText("/page");
});

test("a viewer sees alerts but can't act on them", async ({ page }) => {
  await signInAs(page, viewer, "/alerts");
  await expect(page.getByRole("region", { name: "Open alerts" })).toContainText("orders");
  await expect(page.getByRole("button", { name: /Acknowledge|Silence/ })).toHaveCount(0);
  await expect(page.getByRole("region", { name: "Channels" })).toHaveCount(0);
  const refused = await page.request.post("/api/v1/alerts/1/ack", { headers: { "X-Ductus": "1" } });
  expect(refused.status()).toBe(403);
});

test("no accessibility violations on the alert screens in any theme", async ({ page }) => {
  test.setTimeout(240_000);
  await signInAs(page, operator);
  for (const t of ["light", "dark", "contrast"]) {
    await page.request.put("/api/v1/me/settings", { headers: { "X-Ductus": "1" }, data: { theme: t } });
    await page.evaluate((name) => localStorage.setItem("ductus.theme", name), t);
    const tags = t === "contrast" ? [...AA_TAGS, "wcag2aaa"] : AA_TAGS;
    for (const path of ["/alerts", "/", "/interfaces/adt"]) {
      await page.goto(path);
      await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
      await expect(page.getByText("Loading…")).toHaveCount(0);
      const { violations } = await new AxeBuilder({ page }).withTags(tags).analyze();
      const found = violations.map((v) => `${v.id}: ${v.nodes.map((n) => n.target.join(" ")).join(", ")}`);
      expect(found, `${t}, ${path}`).toEqual([]);
    }
    await page.goto("/alerts");
    await page.getByRole("button", { name: "Silence… adt → lab" }).click();
    await expect(page.getByRole("dialog")).toBeVisible();
    const { violations } = await new AxeBuilder({ page }).withTags(tags).analyze();
    expect(
      violations.map((v) => v.id),
      `${t}, silence dialog`,
    ).toEqual([]);
  }
  await page.request.put("/api/v1/me/settings", { headers: { "X-Ductus": "1" }, data: { theme: "system" } });
});

test("the alert screens pass axe in custom colors", async ({ page }) => {
  test.setTimeout(240_000);
  await signInAs(page, operator, "/appearance");
  for (const name of ["Sepia", "Ocean"]) {
    await page.goto("/appearance");
    await page.getByLabel("Start from").selectOption(name);
    await page.getByRole("button", { name: "Use these colors" }).click();
    await expect(page.getByText("These colors are in use.")).toBeVisible();
    for (const path of ["/alerts", "/"]) {
      await page.goto(path);
      await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
      await expect(page.getByText("Loading…")).toHaveCount(0);
      expect(await page.evaluate(() => document.documentElement.dataset.theme)).toBe("custom");
      const { violations } = await new AxeBuilder({ page }).withTags(AA_TAGS).analyze();
      const found = violations.map((v) => `${v.id}: ${v.nodes.map((n) => n.target.join(" ")).join(", ")}`);
      expect(found, `custom ${name}, ${path}`).toEqual([]);
    }
  }
  await page.request.put("/api/v1/me/settings", { headers: { "X-Ductus": "1" }, data: { theme: "system" } });
});

test("the alert screens fit a phone", async ({ browser }) => {
  const { context, page } = await elsewhere(browser, { width: 390, height: 844 });
  await signInAs(page, operator);
  for (const path of ["/alerts", "/", "/interfaces/adt"]) {
    await page.goto(path);
    await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
    await expect(page.getByText("Loading…")).toHaveCount(0);
    const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
    expect(overflow, path).toBeLessThanOrEqual(0);
  }
  await page.goto("/alerts");
  await page.getByRole("button", { name: "Silence… adt → lab" }).click();
  const dialog = page.getByRole("dialog");
  const box = await dialog.getByRole("button", { name: "Silence", exact: true }).boundingBox();
  expect(box && box.x >= 0 && box.x + box.width <= 390).toBe(true);
  await context.close();
});
