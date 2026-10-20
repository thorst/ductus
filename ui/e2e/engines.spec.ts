// Engines sharing one database (scripts/demo-ui.sh): a second engine runs another project
// (`ui-neighbour`), and the demo's own project was started a second time and refused. The Overview
// lists the other engine and what it runs; the audit log says why the start was refused. Both pass
// axe in every theme (AAA in High contrast), in custom colors (presets and a bad palette after Fix
// all), and fit a phone. Runs last (playwright.config.ts).

import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page } from "@playwright/test";

function person(key: string) {
  const [user = "", ...rest] = (process.env[key] ?? "").split(":");
  return { user, password: rest.join(":") };
}

// Reads the audit log, and nothing beyond a viewer otherwise.
const auditor = person("DUCTUS_PERSON_AUDITOR");

async function signIn(page: Page, path = "/") {
  await page.goto(path);
  await expect(page).toHaveURL(/\/sign-in/);
  await page.getByLabel("Username").fill(auditor.user);
  await page.getByLabel("Password").fill(auditor.password);
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page).not.toHaveURL(/\/sign-in/);
  await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
}

const AA_TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"];
const PAGES = ["/", "/audit?action=engine.refused"];

async function ready(page: Page, path: string) {
  await page.goto(path);
  await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
  await expect(page.getByText("Loading…")).toHaveCount(0);
  if (path === "/") await expect(page.getByRole("region", { name: "Other engines on this database" })).toBeVisible();
}

async function axe(page: Page, tags: string[], what: string) {
  const { violations } = await new AxeBuilder({ page }).withTags(tags).analyze();
  const found = violations.map((v) => `${v.id}: ${v.nodes.map((n) => n.target.join(" ")).join(", ")}`);
  expect(found, what).toEqual([]);
}

test("the overview lists the other engine on this database and what it runs", async ({ page }) => {
  await signIn(page);
  const card = page.getByRole("region", { name: "Other engines on this database" });
  const engine = card.getByRole("listitem");
  await expect(engine).toHaveCount(1);
  await expect(engine).toContainText("Runs ui-neighbour");
  await expect(engine).toContainText(/Ductus .* · dev · since .* · pid \d+/);
  await page.screenshot({ path: "test-results/overview-other-engines.png", fullPage: true });
});

test("the audit log says why a start was refused", async ({ page }) => {
  await signIn(page, "/audit?action=engine.refused");
  const log = page.getByRole("table", { name: "Audit log" });
  const row = log.getByRole("row", { name: /Engine start refused/ }).first();
  await expect(row).toContainText("another engine runs adt, billing, orders on");
  await expect(row).toContainText(/\(pid \d+\)/);
});

test("the engines screens pass axe in every theme and fit a phone", async ({ browser }) => {
  test.setTimeout(240_000);
  const context = await browser.newContext({ viewport: { width: 1360, height: 900 } });
  const page = await context.newPage();
  await signIn(page);
  for (const t of ["light", "dark", "contrast"]) {
    await page.request.put("/api/v1/me/settings", { headers: { "X-Ductus": "1" }, data: { theme: t } });
    await page.evaluate((name) => localStorage.setItem("ductus.theme", name), t);
    for (const path of PAGES) {
      await ready(page, path);
      await axe(page, t === "contrast" ? [...AA_TAGS, "wcag2aaa"] : AA_TAGS, `${t}, ${path}`);
    }
  }
  for (const name of ["Sepia", "Ocean"]) {
    await page.goto("/appearance");
    await page.getByLabel("Start from").selectOption(name);
    await page.getByRole("button", { name: "Use these colors" }).click();
    await expect(page.getByText("These colors are in use.")).toBeVisible();
    for (const path of PAGES) {
      await ready(page, path);
      await axe(page, AA_TAGS, `custom ${name}, ${path}`);
    }
  }
  // A palette that can't be read (pale grey text, a yellow accent on white), after Fix all.
  await page.goto("/appearance");
  const colors = {
    Header: "#ffffff",
    Accent: "#ffeb3b",
    "Page background": "#ffffff",
    Panel: "#ffffff",
    Text: "#cccccc",
  };
  for (const [label, hex] of Object.entries(colors)) await page.getByLabel(`${label} as hex`).fill(hex);
  await page.getByRole("button", { name: "Fix all" }).click();
  await expect(page.getByRole("status").first()).toContainText("Every pair reaches WCAG AA");
  await page.getByRole("button", { name: "Use these colors" }).click();
  for (const path of PAGES) {
    await ready(page, path);
    await axe(page, AA_TAGS, `fixed custom colors, ${path}`);
  }
  await page.request.put("/api/v1/me/settings", { headers: { "X-Ductus": "1" }, data: { theme: "system" } });
  await context.close();

  const phone = await browser.newContext({ viewport: { width: 390, height: 844 } });
  const small = await phone.newPage();
  await signIn(small);
  for (const path of PAGES) {
    await ready(small, path);
    const overflow = await small.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
    expect(overflow, path).toBeLessThanOrEqual(0);
  }
  await phone.close();
});
