// The web UI against a real engine (scripts/demo-ui.sh sets it up: adt's lab behind, orders
// quarantining patient 100003, billing paused).

import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page } from "@playwright/test";

const token = process.env.DUCTUS_VIEWER_TOKEN ?? "";

async function signIn(page: Page, path = "/") {
  await page.goto(path);
  await expect(page).toHaveURL(/\/sign-in/);
  await page.getByLabel("Token").fill(token);
  await page.getByRole("button", { name: "Sign in" }).click();
}

async function theme(page: Page, name: string) {
  await page.evaluate((t) => localStorage.setItem("ductus.theme", t), name);
  await page.reload();
}

test("a wrong token is refused; the right one signs in and comes back to the page asked for", async ({
  page,
  context,
}) => {
  await page.goto("/interfaces/adt");
  await expect(page).toHaveURL(/\/sign-in\?redirect=/);
  await page.getByLabel("Token").fill("dux_wrong");
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page.getByRole("alert")).toContainText("unknown or revoked token");

  await page.getByLabel("Token").fill(token);
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page).toHaveURL(/\/interfaces\/adt$/);
  await expect(page.getByRole("heading", { level: 1 })).toContainText("adt");

  // The session is a cookie page scripts can't read; the token isn't kept anywhere.
  const cookie = (await context.cookies()).find((c) => c.name === "ductus_session");
  expect(cookie).toMatchObject({ httpOnly: true, sameSite: "Strict", path: "/api" });
  expect(await page.evaluate(() => document.cookie)).not.toContain("ductus_session");
  expect(await page.evaluate(() => JSON.stringify(localStorage))).not.toContain("dux_");
});

test("the overview says what needs attention", async ({ page }) => {
  await signIn(page);
  const attention = page.getByRole("region", { name: "Needs attention" });
  await expect(attention.getByRole("listitem").filter({ hasText: "adt → lab" })).toContainText("Behind");
  await expect(attention.getByRole("listitem").filter({ hasText: "orders" })).toContainText("Errors");
  await expect(attention.getByRole("listitem").filter({ hasText: "billing" })).toContainText("Paused");
  await expect(attention).not.toContainText("→ default");
  await expect(page.getByRole("definition").filter({ hasText: /^4$/ })).toBeVisible();
  await expect(page.getByText("Live", { exact: true })).toBeVisible();
  for (const t of ["light", "dark", "contrast"]) {
    await theme(page, t);
    await expect(attention).toBeVisible();
    await page.screenshot({ path: `test-results/overview-${t}.png`, fullPage: true });
  }
});

test("interfaces can be filtered", async ({ page }) => {
  await signIn(page, "/interfaces");
  const rows = page.locator("tbody tr");
  await expect(rows).toHaveCount(3);
  await page.getByLabel("Filter interfaces").fill("orders team");
  await expect(rows).toHaveCount(1);
  await expect(rows.first()).toContainText("orders");
  await page.getByLabel("Filter interfaces").fill("");
  await page.getByLabel("Only those needing attention").check();
  await expect(rows).toHaveCount(2);
  await expect(page.locator("tbody")).not.toContainText("billing");
  await rows.filter({ hasText: "adt" }).click();
  await expect(page).toHaveURL(/\/interfaces\/adt$/);
});

test("a destination's state, its last error and a pause with its reason", async ({ page }) => {
  await signIn(page, "/interfaces/adt");
  const lab = page.getByRole("region", { name: "lab", exact: true });
  await expect(lab).toContainText("Behind");
  await expect(lab).toContainText("Last error");
  await expect(page.getByRole("region", { name: "archive" })).toContainText("OK");
  await expect(page.getByText("Epic (Epic Systems)")).toBeVisible();
  await page.screenshot({ path: "test-results/interface-adt.png", fullPage: true });

  await page.goto("/interfaces/billing");
  await expect(page.getByText("invoice system upgrade tonight")).toBeVisible();
});

test("a quarantined message: why, what each destination did, and its HL7 fields", async ({ page }) => {
  await signIn(page, "/interfaces/orders?tab=messages");
  await page.getByRole("button", { name: "quarantined" }).click();
  await expect(page).toHaveURL(/status=quarantined/);
  const rows = page.getByRole("table", { name: "orders messages" }).getByRole("row");
  await expect(rows).toHaveCount(5); // header + 4
  await rows.nth(1).getByRole("link").click();

  await expect(page.getByRole("heading", { level: 1 })).toContainText("quarantined");
  await expect(page.getByText("unknown patient 100003 in the order system").first()).toBeVisible();
  await page.locator("[data-path='PID-3.1']").hover();
  await expect(page.getByText("PID-3.1", { exact: true })).toBeVisible();
  await page.screenshot({ path: "test-results/message.png", fullPage: true });
  await page.getByRole("button", { name: "Raw" }).click();
  await expect(page.locator("pre")).toContainText("PID|1||100003^^^MRN");
});

test("no accessibility violations in any theme", async ({ page }) => {
  await signIn(page);
  const pages = ["/", "/interfaces", "/interfaces/adt", "/interfaces/orders?tab=messages"];
  for (const t of ["light", "dark", "contrast"]) {
    await theme(page, t);
    for (const path of pages) {
      await page.goto(path);
      await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
      // High contrast promises AAA (7:1); the others AA.
      const tags = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", ...(t === "contrast" ? ["wcag2aaa"] : [])];
      const { violations } = await new AxeBuilder({ page }).withTags(tags).analyze();
      const found = violations.map((v) => `${v.id}: ${v.nodes.map((n) => n.target.join(" ")).join(", ")}`);
      expect(found, `${t} theme, ${path}`).toEqual([]);
    }
  }
});

test("a phone-sized screen gets the navigation on top and no sideways page scroll", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await signIn(page);
  for (const path of ["/", "/interfaces", "/interfaces/adt", "/interfaces/orders?tab=messages"]) {
    await page.goto(path);
    await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
    await expect(
      page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: "Interfaces" }),
    ).toBeVisible();
    const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
    expect(overflow, path).toBeLessThanOrEqual(0);
  }
  await page.screenshot({ path: "test-results/phone-messages.png" });
});

test("signing out ends the session", async ({ page }) => {
  await signIn(page);
  await page.getByRole("button", { name: /viewer/ }).click();
  await page.getByRole("menuitem", { name: "Sign out" }).click();
  await expect(page).toHaveURL(/\/sign-in/);
  await page.goto("/interfaces");
  await expect(page).toHaveURL(/\/sign-in/);
});
