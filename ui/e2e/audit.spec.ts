// The audit log and dismissing quarantined messages (scripts/demo-ui.sh makes the accounts): an
// operator dismisses a quarantined delivery with a reason, a person with the audit permission finds
// it in the log (and who looked at the message), a viewer without it is told it isn't theirs, and
// every changed screen passes axe in every theme (AAA in High contrast) and fits a phone. Runs after
// the people tests (playwright.config.ts).

import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Browser, type Page } from "@playwright/test";

/** "name:password" from the demo's environment. */
function person(key: string) {
  const [user = "", ...rest] = (process.env[key] ?? "").split(":");
  return { user, password: rest.join(":") };
}

const operator = person("DUCTUS_PERSON_OPERATOR");
const viewer = person("DUCTUS_PERSON_VIEWER");
const auditor = person("DUCTUS_PERSON_AUDITOR");

type Who = { user: string; password: string };

async function signInAs(page: Page, who: Who, path = "/") {
  await page.goto(path);
  await expect(page).toHaveURL(/\/sign-in/);
  await page.getByLabel("Username").fill(who.user);
  await page.getByLabel("Password").fill(who.password);
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page).not.toHaveURL(/\/sign-in/);
  await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
}

/** A browser of its own: cookies and storage apart from every other. */
async function elsewhere(browser: Browser, viewport = { width: 1360, height: 900 }) {
  const context = await browser.newContext({ viewport });
  return { context, page: await context.newPage() };
}

const AA_TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"];
const REASON = "unit retired by the lab";

// Filled by the dismissal test, read by the ones after it.
let dismissed = 0;

test("an operator dismisses a quarantined delivery, and a reason is required", async ({ page }) => {
  await signInAs(page, operator, "/quarantine");
  const table = page.getByRole("table", { name: "Quarantined deliveries" });
  await expect(table).toBeVisible();
  const rows = table.locator("tbody tr");
  const before = await rows.count();
  expect(before).toBeGreaterThan(0);
  const first = rows.first();
  dismissed = Number(await first.getByRole("link").first().innerText());
  await expect(page.getByRole("button", { name: /^Dismiss/ })).toBeDisabled();
  await first.getByRole("checkbox").check();
  await page.getByRole("button", { name: "Dismiss 1 selected" }).click();

  const dialog = page.getByRole("dialog");
  await expect(dialog).toContainText("Dismiss 1 delivery of 1 message for 1 destination?");
  await expect(dialog).toContainText("They stay in the message history and in the audit log");
  const confirm = dialog.getByRole("button", { name: "Dismiss", exact: true });
  await expect(confirm).toBeDisabled();
  await dialog.getByLabel("Reason (required, kept in the audit log)").fill("   ");
  await expect(confirm).toBeDisabled();
  await page.screenshot({ path: "test-results/dismiss-dialog.png" });
  await dialog.getByLabel("Reason (required, kept in the audit log)").fill(REASON);
  await confirm.click();

  await expect(page.getByRole("status")).toHaveText("Dismissed 1 delivery.");
  await expect(rows).toHaveCount(before - 1);
  await expect(table.getByRole("link", { name: String(dismissed), exact: true })).toHaveCount(0);

  // The message shows what happened, and the delivery stays replayable.
  await page.goto(`/messages/${dismissed}`);
  const history = page.getByRole("region", { name: "History" });
  await expect(history).toContainText("Dismissed");
  await expect(history).toContainText(REASON);
  await expect(history).toContainText(operator.user);
  const destinations = page.getByRole("region", { name: "Destinations" });
  await expect(destinations).toContainText(`dismissed by ${operator.user}: ${REASON}`);
  await expect(destinations.getByRole("button", { name: /Replay/ })).toBeVisible();
  await page.screenshot({ path: "test-results/dismissed-message.png", fullPage: true });
});

test("a viewer can't dismiss, and without the audit permission the log isn't theirs", async ({ page }) => {
  await signInAs(page, viewer, "/quarantine");
  await expect(page.getByRole("button", { name: /Dismiss/ })).toHaveCount(0);
  await expect(page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: "Audit" })).toHaveCount(0);
  await page.goto("/audit");
  await expect(page.getByText("Reading the audit log needs the audit permission")).toBeVisible();
  const refused = await page.request.get("/api/v1/audit");
  expect(refused.status()).toBe(403);
  // Their own activity is theirs.
  await page.goto("/account");
  const mine = page.getByRole("region", { name: "My activity" });
  await expect(mine).toContainText("Signed in");
  await expect(mine).not.toContainText(operator.user);
});

test("the audit permission opens the log: who dismissed what, who looked at it, and an export", async ({
  page,
  browser,
}) => {
  // The operator reads the message (that is a row), the auditor looks for it.
  const op = await elsewhere(browser);
  await signInAs(op.page, operator, `/messages/${dismissed}`);
  await expect(op.page.locator("[data-path]").first()).toBeVisible();
  await expect(op.page.getByRole("region", { name: "Who viewed the payload" })).toHaveCount(0);
  await op.context.close();

  await signInAs(page, auditor, `/messages/${dismissed}`);
  const readers = page.getByRole("region", { name: "Who viewed the payload" });
  await expect(readers).toContainText(operator.user);
  await expect(readers).toContainText("web UI");
  // They may see that it was read, not read it.
  await expect(page.getByText("You don’t have permission to read payloads")).toBeVisible();

  const nav = page.getByRole("navigation", { name: "Main" });
  await nav.getByRole("link", { name: "Audit" }).click();
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Audit log");
  await page.getByLabel("What").selectOption("message.dismiss");
  await page.getByLabel("Message id").fill(String(dismissed));
  await page.getByRole("button", { name: "Filter" }).click();
  const table = page.getByRole("table", { name: "Audit log" });
  const row = table.locator("tbody tr").filter({ hasText: operator.user });
  await expect(row).toHaveCount(1);
  await expect(row).toContainText("Dismissed a quarantined message");
  await expect(row).toContainText(REASON);
  await expect(row).toContainText("web UI");
  await expect(row.getByRole("link", { name: `message ${dismissed}` })).toBeVisible();
  expect(page.url()).toContain("action=message.dismiss");
  expect(page.url()).toContain(`message=${dismissed}`);
  await page.screenshot({ path: "test-results/audit.png", fullPage: true });

  // Every sign-in and read is in it too; a failed sign-in says why and keeps no password.
  await page.getByLabel("What").selectOption("session.failed");
  await page.getByLabel("Message id").fill("");
  await page.getByRole("button", { name: "Filter" }).click();
  await expect(table).toContainText("Failed sign-in");
  await expect(table).not.toContainText("nobody-has-this-name");

  // The export is what the page lists, as JSON lines. Filtered to this message: an export runs
  // oldest first, 10,000 at a time, and a long-lived dev database holds more dismissals than that.
  await page.getByLabel("What").selectOption("message.dismiss");
  await page.getByLabel("Message id").fill(String(dismissed));
  await page.getByRole("button", { name: "Filter" }).click();
  const link = page.getByRole("link", { name: /Export these/ });
  const href = (await link.getAttribute("href")) ?? "";
  expect(href).toContain("format=ndjson");
  const exported = await page.request.get(href);
  expect(exported.status()).toBe(200);
  expect(exported.headers()["content-type"]).toBe("application/x-ndjson");
  const lines = (await exported.text())
    .trim()
    .split("\n")
    .map((l) => JSON.parse(l));
  expect(lines.length).toBeGreaterThan(0);
  expect(lines.every((l) => l.action === "message.dismiss" && l.message === dismissed)).toBe(true);
  expect(lines.some((l) => l.message === dismissed && l.detail.reason === REASON)).toBe(true);
});

test("an operator sees their own activity", async ({ page }) => {
  await signInAs(page, operator, "/account");
  const mine = page.getByRole("region", { name: "My activity" });
  await expect(mine).toContainText("Dismissed a quarantined message");
  await expect(mine).toContainText(REASON);
  await expect(mine).toContainText("Viewed a payload");
});

/** The screens this part changed or added, signed in as the auditor, in every theme. */
test("no accessibility violations on the audit screens in any theme", async ({ page }) => {
  test.setTimeout(240_000);
  await signInAs(page, auditor);
  for (const t of ["light", "dark", "contrast"]) {
    // On the server as well as here: a reload applies the account's choice.
    await page.request.put("/api/v1/me/settings", { headers: { "X-Ductus": "1" }, data: { theme: t } });
    await page.evaluate((name) => localStorage.setItem("ductus.theme", name), t);
    for (const path of [
      "/audit",
      `/audit?action=message.dismiss&message=${dismissed}`,
      `/messages/${dismissed}`,
      "/account",
    ]) {
      await page.goto(path);
      await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
      await expect(page.getByText("Loading…")).toHaveCount(0);
      const tags = t === "contrast" ? [...AA_TAGS, "wcag2aaa"] : AA_TAGS;
      const { violations } = await new AxeBuilder({ page }).withTags(tags).analyze();
      const found = violations.map((v) => `${v.id}: ${v.nodes.map((n) => n.target.join(" ")).join(", ")}`);
      expect(found, `${t}, ${path}`).toEqual([]);
    }
  }
  await page.request.put("/api/v1/me/settings", { headers: { "X-Ductus": "1" }, data: { theme: "system" } });
});

test("the quarantine and its dismiss dialog pass axe in every theme", async ({ page }) => {
  test.setTimeout(240_000);
  await signInAs(page, operator);
  for (const t of ["light", "dark", "contrast"]) {
    await page.request.put("/api/v1/me/settings", { headers: { "X-Ductus": "1" }, data: { theme: t } });
    await page.evaluate((name) => localStorage.setItem("ductus.theme", name), t);
    await page.goto("/quarantine");
    await expect(page.getByRole("table", { name: "Quarantined deliveries" })).toBeVisible();
    await page
      .getByRole("checkbox", { name: /Select message/ })
      .first()
      .check();
    const tags = t === "contrast" ? [...AA_TAGS, "wcag2aaa"] : AA_TAGS;
    let result = await new AxeBuilder({ page }).withTags(tags).analyze();
    expect(
      result.violations.map((v) => v.id),
      `${t}, quarantine`,
    ).toEqual([]);
    await page.getByRole("button", { name: "Dismiss 1 selected" }).click();
    await expect(page.getByRole("dialog")).toBeVisible();
    result = await new AxeBuilder({ page }).withTags(tags).analyze();
    expect(
      result.violations.map((v) => v.id),
      `${t}, dismiss dialog`,
    ).toEqual([]);
  }
  await page.request.put("/api/v1/me/settings", { headers: { "X-Ductus": "1" }, data: { theme: "system" } });
});

test("the audit screens fit a phone", async ({ browser }) => {
  const { context, page } = await elsewhere(browser, { width: 390, height: 844 });
  await signInAs(page, auditor);
  for (const path of ["/audit", `/messages/${dismissed}`, "/account"]) {
    await page.goto(path);
    await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
    await expect(page.getByText("Loading…")).toHaveCount(0);
    const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
    expect(overflow, path).toBeLessThanOrEqual(0);
  }
  await context.close();
  const op = await elsewhere(browser, { width: 390, height: 844 });
  await signInAs(op.page, operator, "/quarantine");
  await expect(op.page.getByRole("table", { name: "Quarantined deliveries" })).toBeVisible();
  await op.page
    .getByRole("checkbox", { name: /Select message/ })
    .first()
    .check();
  await op.page.getByRole("button", { name: "Dismiss 1 selected" }).click();
  const dialog = op.page.getByRole("dialog");
  await expect(dialog).toBeVisible();
  // The whole dialog is reachable: its buttons are inside the screen.
  const box = await dialog.getByRole("button", { name: "Dismiss", exact: true }).boundingBox();
  expect(box && box.x >= 0 && box.x + box.width <= 390).toBe(true);
  const overflow = await op.page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
  expect(overflow).toBeLessThanOrEqual(0);
  await op.context.close();
});
