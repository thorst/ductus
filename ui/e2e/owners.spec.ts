// Owners matched to people (scripts/demo-ui.sh): adt's archive is owned by "Olaf Night", the
// display name of the directory operator test-user-olaf (whose account the directory tests made),
// so their Interfaces page opens on "Mine". The teams that own the rest have no account: the
// Owners page says so, and that a move to someone without an account would reach nobody.

import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page } from "@playwright/test";

function person(key: string) {
  const [user = "", ...rest] = (process.env[key] ?? "").split(":");
  return { user, password: rest.join(":") };
}

const olaf = person("DUCTUS_DIRECTORY_OPERATOR");

async function signIn(page: Page, path = "/") {
  await page.goto(path);
  await expect(page).toHaveURL(/\/sign-in/);
  await page.getByLabel("Username").fill(olaf.user);
  await page.getByLabel("Password").fill(olaf.password);
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page).not.toHaveURL(/\/sign-in/);
  await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
}

const AA_TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"];

async function axe(page: Page, tags: string[], what: string) {
  await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
  await expect(page.getByText("Loading…")).toHaveCount(0);
  const { violations } = await new AxeBuilder({ page }).withTags(tags).analyze();
  const found = violations.map((v) => `${v.id}: ${v.nodes.map((n) => n.target.join(" ")).join(", ")}`);
  expect(found, what).toEqual([]);
}

test("an operator's Interfaces page opens on their own", async ({ page }) => {
  await signIn(page, "/interfaces");
  const whose = page.getByRole("group", { name: "Whose interfaces" });
  await expect(whose.getByRole("button", { name: "Mine (1)" })).toHaveAttribute("aria-pressed", "true");
  const table = page.getByRole("table");
  await expect(table.getByRole("link", { name: "adt", exact: true })).toBeVisible();
  await expect(table.getByRole("link", { name: "orders", exact: true })).toHaveCount(0);
  await page.screenshot({ path: "test-results/interfaces-mine.png", fullPage: true });

  await whose.getByRole("button", { name: "All" }).click();
  await expect(page).toHaveURL(/mine=false/);
  await expect(table.getByRole("link", { name: "orders", exact: true })).toBeVisible();
  await expect(table.getByRole("link", { name: "billing", exact: true })).toBeVisible();
});

test("the Owners page says who has an account, and the interface page too", async ({ page }) => {
  await signIn(page, "/owners");
  const owners = page.getByRole("region", { name: "Owners", exact: true });
  const mine = owners.getByRole("listitem", { name: "Olaf Night" });
  await expect(mine).toContainText(`Olaf Night (${olaf.user}) · you`);
  await expect(mine.getByRole("link", { name: "olaf@example.org" })).toHaveAttribute("href", "mailto:olaf@example.org");
  await expect(mine).toContainText("adt → archive");
  await expect(owners.getByRole("listitem", { name: "ADT team" })).toContainText("Not a user, not a team in owners:");
  // A resources.yaml entry's owner is listed with the interfaces.
  await expect(owners.getByRole("listitem", { name: "Directory Services" })).toContainText("directory corp-directory");
  await expect(owners.getByRole("status")).toContainText("owners reach nobody or the wrong person");
  await page.screenshot({ path: "test-results/owners-people.png", fullPage: true });

  // Moving the directory's ownership to a person with an account: resources.yaml changes.
  const move = page.getByRole("region", { name: "Move ownership" });
  await move.locator("select").selectOption("Directory Services");
  const to = move.getByRole("combobox", { name: "To" });
  await to.fill("Olaf Night");
  await move.getByRole("button", { name: "Preview" }).click();
  const preview = page.getByRole("region", { name: "Preview" });
  await expect(preview).toContainText("1 owner value in 1 file would change.");
  await expect(preview).toContainText("resources.yaml");
  await expect(preview).toContainText("The new owner is Olaf Night (olaf@example.org), who has an account.");
  await expect(preview).toContainText("ductus owners reassign --from 'Directory Services' --to 'Olaf Night'");
  // Someone who isn't an account: said before anyone applies it.
  await to.fill("Night shift");
  await move.getByRole("button", { name: "Preview" }).click();
  await expect(preview.getByRole("alert")).toContainText("isn't an account or a team in `owners:`");

  await page.goto("/interfaces/adt");
  await expect(page.getByText("ADT team")).toBeVisible();
  await expect(page.getByText("Not a user, not a team in owners:")).toBeVisible();
});

test("the owners screens pass axe in every theme and fit a phone", async ({ browser }) => {
  test.setTimeout(120_000);
  const context = await browser.newContext({ viewport: { width: 1360, height: 900 } });
  const page = await context.newPage();
  await signIn(page);
  for (const t of ["light", "dark", "contrast"]) {
    await page.request.put("/api/v1/me/settings", { headers: { "X-Ductus": "1" }, data: { theme: t } });
    await page.evaluate((name) => localStorage.setItem("ductus.theme", name), t);
    for (const path of ["/interfaces", "/owners", "/interfaces/adt"]) {
      await page.goto(path);
      await axe(page, t === "contrast" ? [...AA_TAGS, "wcag2aaa"] : AA_TAGS, `${t}, ${path}`);
    }
  }
  for (const name of ["Sepia", "Ocean"]) {
    await page.goto("/appearance");
    await page.getByLabel("Start from").selectOption(name);
    await page.getByRole("button", { name: "Use these colors" }).click();
    await expect(page.getByText("These colors are in use.")).toBeVisible();
    for (const path of ["/interfaces", "/owners"]) {
      await page.goto(path);
      await axe(page, AA_TAGS, `custom ${name}, ${path}`);
    }
  }
  await page.request.put("/api/v1/me/settings", { headers: { "X-Ductus": "1" }, data: { theme: "system" } });
  await context.close();

  const phone = await browser.newContext({ viewport: { width: 390, height: 844 } });
  const small = await phone.newPage();
  await signIn(small);
  for (const path of ["/interfaces", "/owners"]) {
    await small.goto(path);
    await expect(small.getByText("Loading…")).toHaveCount(0);
    const overflow = await small.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
    expect(overflow, path).toBeLessThanOrEqual(0);
  }
  await phone.close();
});
