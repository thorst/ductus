// Certificates (scripts/demo-ui.sh starts the servers and checks them once): the Connections page
// lists every certificate the engine connects to, soonest first, with days left in words and an
// icon; "Check now" reads them again; the Overview lists the ones to renew; a viewer only looks.
// Both screens pass axe in every theme (AAA in High contrast) and in custom colors, and fit a phone.
// Runs last (playwright.config.ts).

import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page } from "@playwright/test";

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

const AA_TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"];

/** The rows of the Certificates table, top to bottom, as (resource, days-left words). */
async function rows(page: Page) {
  const table = page.getByRole("region", { name: "Certificates" }).getByRole("table", { name: "Certificates" });
  await expect(table).toBeVisible();
  const out: [string, string][] = [];
  for (const row of await table.locator("tbody tr").all()) {
    const cells = row.locator("td");
    const server = (await cells.nth(1).innerText()).split("\n");
    out.push([server[1] ?? "", (await cells.nth(0).innerText()).trim()]);
  }
  return out;
}

test("the Connections page lists every certificate, soonest first, in words", async ({ page }) => {
  await signInAs(page, operator, "/connections");
  expect(await rows(page)).toEqual([
    ["legacy-portal", "Expired 2 days ago"],
    ["hie", "3 days left"],
    ["payer", "10 days left"],
    ["radiology", "20 days left"],
    ["pharmacy", "40 days left"],
    ["fax-gateway", "Not read yet"],
  ]);
  const card = page.getByRole("region", { name: "Certificates" });
  // Self-signed: read anyway, and said not to be trusted.
  await expect(card.getByRole("row", { name: /hie/ })).toContainText("not trusted by this server");
  await expect(card.getByRole("row", { name: /fax-gateway/ })).toContainText("refused");
  await expect(card).toContainText(/engine checks every 6 h/i);
  await expect(card).toContainText("No alert rule names certificate");
  await page.screenshot({ path: "test-results/certificates.png", fullPage: true });

  await card.getByRole("button", { name: "Check now" }).click();
  await expect(card.getByRole("status")).toHaveText("Checked 6: 5 certificates read.", { timeout: 20_000 });
  // The check is kept: the report below is the one just run (by this person).
  await page.reload();
  await expect(page.getByText(/The last check: .*certificates only, asked for by test-user-ui-operator/)).toBeVisible();
});

test("the overview lists the certificates to renew", async ({ page }) => {
  await signInAs(page, operator);
  const card = page.getByRole("region", { name: "Certificates expiring" });
  const items = card.getByRole("listitem");
  await expect(items).toHaveCount(4); // 40 days is fine; the one never read isn't listed
  await expect(items.first()).toContainText("Expired 2 days ago");
  await expect(items.first()).toContainText("legacy-portal");
  await expect(card).not.toContainText("pharmacy");
  await card.getByRole("link", { name: "All certificates" }).click();
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Connection checks");
});

test("a viewer sees the certificates but can't check them", async ({ page }) => {
  await signInAs(page, viewer, "/connections");
  expect((await rows(page)).length).toBe(6);
  await expect(page.getByRole("button", { name: "Check now" })).toHaveCount(0);
});

test("the certificate screens pass axe in every theme", async ({ page }) => {
  test.setTimeout(120_000);
  await signInAs(page, operator);
  for (const t of ["light", "dark", "contrast"]) {
    await page.request.put("/api/v1/me/settings", { headers: { "X-Ductus": "1" }, data: { theme: t } });
    await page.evaluate((name) => localStorage.setItem("ductus.theme", name), t);
    const tags = t === "contrast" ? [...AA_TAGS, "wcag2aaa"] : AA_TAGS;
    for (const path of ["/connections", "/"]) {
      await page.goto(path);
      await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
      await expect(page.getByText("Loading…")).toHaveCount(0);
      await expect(
        page.getByRole("region", { name: path === "/" ? "Certificates expiring" : "Certificates" }),
      ).toBeVisible();
      const { violations } = await new AxeBuilder({ page }).withTags(tags).analyze();
      const found = violations.map((v) => `${v.id}: ${v.nodes.map((n) => n.target.join(" ")).join(", ")}`);
      expect(found, `${t}, ${path}`).toEqual([]);
    }
  }
  await page.request.put("/api/v1/me/settings", { headers: { "X-Ductus": "1" }, data: { theme: "system" } });
});

test("the certificate screens pass axe in custom colors", async ({ page }) => {
  test.setTimeout(120_000);
  await signInAs(page, operator, "/appearance");
  for (const name of ["Sepia", "Ocean"]) {
    await page.goto("/appearance");
    await page.getByLabel("Start from").selectOption(name);
    await page.getByRole("button", { name: "Use these colors" }).click();
    await expect(page.getByText("These colors are in use.")).toBeVisible();
    for (const path of ["/connections", "/"]) {
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

test("the certificate screens fit a phone", async ({ browser }) => {
  const context = await browser.newContext({ viewport: { width: 390, height: 844 } });
  const page = await context.newPage();
  await signInAs(page, operator);
  for (const path of ["/connections", "/"]) {
    await page.goto(path);
    await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
    await expect(page.getByText("Loading…")).toHaveCount(0);
    const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
    expect(overflow, path).toBeLessThanOrEqual(0);
  }
  // Check now is reachable on a phone.
  const box = await page.goto("/connections").then(() => page.getByRole("button", { name: "Check now" }).boundingBox());
  expect(box && box.x >= 0 && box.x + box.width <= 390).toBe(true);
  await context.close();
});
