// People who sign in with the directory (scripts/demo-ui.sh starts a throwaway OpenLDAP and sets
// auth.ldap): the same sign-in page, refusals that say nothing about who exists, an account whose
// password, name, email and role are the directory's (read-only on Account and on the Users page,
// where an admin can still disable it), and both screens in every theme (AAA in High contrast),
// in custom colors and at phone width. Runs last (playwright.config.ts): it makes an account.

import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page } from "@playwright/test";

function person(key: string) {
  const [user = "", ...rest] = (process.env[key] ?? "").split(":");
  return { user, password: rest.join(":") };
}

const admin = person("DUCTUS_PERSON_ADMIN");
const olaf = person("DUCTUS_DIRECTORY_OPERATOR");
const nobody = person("DUCTUS_DIRECTORY_NOBODY");

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

const AA_TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"];

async function axe(page: Page, tags: string[], what: string) {
  await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
  await expect(page.getByText("Loading…")).toHaveCount(0);
  const { violations } = await new AxeBuilder({ page }).withTags(tags).analyze();
  const found = violations.map((v) => `${v.id}: ${v.nodes.map((n) => n.target.join(" ")).join(", ")}`);
  expect(found, what).toEqual([]);
}

test("a directory person signs in with their directory password", async ({ page }) => {
  await page.goto("/");
  await expect(page).toHaveURL(/\/sign-in/);
  const refused = page.getByRole("alert");
  // A wrong password and a name nobody has: the same answer, as for local accounts.
  await page.getByLabel("Username").fill(olaf.user);
  await page.getByLabel("Password").fill("not the password");
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(refused).toHaveText("wrong username or password");
  // The right password, but in no Ductus group: told so (the password was right).
  await page.getByLabel("Username").fill(nobody.user);
  await page.getByLabel("Password").fill(nobody.password);
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(refused).toContainText("no access to Ductus");

  await page.getByLabel("Username").fill(olaf.user);
  await page.getByLabel("Password").fill(olaf.password);
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page).not.toHaveURL(/\/sign-in/);
  // The role comes from the groups: operator.
  await expect(page.getByRole("button", { name: new RegExp(`${olaf.user}.*operator`) })).toBeVisible();

  await page.goto("/account");
  const password = page.getByRole("region", { name: "Password" });
  await expect(password).toContainText("your password is your directory password");
  await expect(password.getByRole("button", { name: "Change password" })).toHaveCount(0);
  const details = page.getByRole("region", { name: "Details" });
  await expect(details).toContainText("Olaf Night");
  await expect(details).toContainText("olaf@example.org");
  await expect(details.getByRole("textbox")).toHaveCount(0);
  await expect(page.getByRole("region", { name: "What you can do" })).toContainText(
    "Your groups in the directory decide these.",
  );
  await page.screenshot({ path: "test-results/directory-account.png", fullPage: true });
});

test("on the Users page a directory account is read-only and can still be disabled", async ({ page }) => {
  await signInAs(page, admin, "/users");
  const table = page.getByRole("table", { name: "Users" });
  const row = table.getByRole("row", { name: new RegExp(olaf.user) });
  await expect(row).toContainText("Directory account");
  await expect(row).toContainText("Olaf Night");
  await expect(row).toContainText("operator");
  // Role, name, email and password are the directory's: no Edit, no Reset password.
  await expect(page.getByRole("button", { name: `Edit ${olaf.user}` })).toHaveCount(0);
  await expect(page.getByRole("button", { name: `Reset password for ${olaf.user}` })).toHaveCount(0);
  await expect(page.getByRole("button", { name: `Edit ${admin.user}` })).toBeVisible();
  await expect(table.getByRole("row", { name: new RegExp(admin.user) })).toContainText("Local account");
  // The API refuses the change too.
  const users = (await (await page.request.get("/api/v1/users")).json()) as { id: number; username: string }[];
  const id = users.find((u) => u.username === olaf.user)?.id;
  const changed = await page.request.patch(`/api/v1/users/${id}`, {
    headers: { "X-Ductus": "1" },
    data: { display_name: "Someone Else" },
  });
  expect(changed.status()).toBe(400);
  expect(await changed.text()).toContain("come from its directory");
  await page.screenshot({ path: "test-results/directory-users.png", fullPage: true });

  // An admin can still shut a directory account out here.
  await page.getByRole("button", { name: `Disable ${olaf.user}` }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Disable" }).click();
  await expect(row).toContainText("Disabled");
  await page.getByRole("button", { name: `Enable ${olaf.user}` }).click();
  await expect(row).toContainText("Active");
});

// demo-ui.sh's legacy-portal is owned by anna@example.org, the directory's Anna Smith (a viewer
// there), who never signs in here: the engine made her account at its first refresh round.
test("an owner in the directory has an account before their first sign-in", async ({ page }) => {
  await signInAs(page, admin, "/users");
  const row = page.getByRole("table", { name: "Users" }).getByRole("row", { name: /test-user-anna\b/ });
  await expect(row).toContainText("Directory account, made because they own something");
  await expect(row).toContainText("Never signed in");
  await expect(row).toContainText("Anna Smith · anna@example.org");
  await expect(row).toContainText("viewer");
  await page.screenshot({ path: "test-results/directory-owner-account.png", fullPage: true });

  await page.goto("/owners");
  const anna = page
    .getByRole("region", { name: "Owners", exact: true })
    .getByRole("listitem", { name: "anna@example.org" });
  await expect(anna).toContainText("Anna Smith (test-user-anna)");
  await expect(anna).toContainText("api legacy-portal");
});

test("the directory account screens pass axe in every theme", async ({ browser }) => {
  test.setTimeout(120_000);
  const contexts = [];
  for (const [who, path] of [
    [admin, "/users"],
    [olaf, "/account"],
  ] as const) {
    const context = await browser.newContext({ viewport: { width: 1360, height: 900 } });
    contexts.push(context);
    const page = await context.newPage();
    await signInAs(page, who, path);
    for (const t of ["light", "dark", "contrast"]) {
      await page.request.put("/api/v1/me/settings", { headers: { "X-Ductus": "1" }, data: { theme: t } });
      await page.evaluate((name) => localStorage.setItem("ductus.theme", name), t);
      await page.goto(path);
      await axe(page, t === "contrast" ? [...AA_TAGS, "wcag2aaa"] : AA_TAGS, `${t}, ${path}`);
    }
    for (const name of ["Sepia", "Ocean"]) {
      await page.goto("/appearance");
      await page.getByLabel("Start from").selectOption(name);
      await page.getByRole("button", { name: "Use these colors" }).click();
      await expect(page.getByText("These colors are in use.")).toBeVisible();
      await page.goto(path);
      expect(await page.evaluate(() => document.documentElement.dataset.theme)).toBe("custom");
      await axe(page, AA_TAGS, `custom ${name}, ${path}`);
    }
    await page.request.put("/api/v1/me/settings", { headers: { "X-Ductus": "1" }, data: { theme: "system" } });
  }
  for (const context of contexts) await context.close();
});

test("the directory account screens fit a phone", async ({ browser }) => {
  for (const [who, path] of [
    [admin, "/users"],
    [olaf, "/account"],
  ] as const) {
    const context = await browser.newContext({ viewport: { width: 390, height: 844 } });
    const page = await context.newPage();
    await signInAs(page, who, path);
    await expect(page.getByText("Loading…")).toHaveCount(0);
    const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
    expect(overflow, path).toBeLessThanOrEqual(0);
    await context.close();
  }
});
