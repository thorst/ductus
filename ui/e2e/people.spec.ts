// People signing in with a username and password (scripts/demo-ui.sh makes the accounts): wrong
// passwords, what each role and permission sees, an admin choosing a password the person has to
// change, appearance that follows the person to another browser, and accessibility of the
// account screens in every theme. Runs last (playwright.config.ts): it creates users.

import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Browser, type Page } from "@playwright/test";

/** "name:password" from the demo's environment. */
function person(key: string) {
  const [user = "", ...rest] = (process.env[key] ?? "").split(":");
  return { user, password: rest.join(":") };
}

const admin = person("DUCTUS_PERSON_ADMIN");
const operator = person("DUCTUS_PERSON_OPERATOR");
const viewer = person("DUCTUS_PERSON_VIEWER");
/** Accounts made here start with this: demo-ui.sh deletes them when it ends, pass or fail (there's no API for it). */
const PREFIX = process.env.DUCTUS_PEOPLE_PREFIX ?? "test-user-ui-";

type Who = { user: string; password: string };

async function signInAs(page: Page, who: Who, path = "/") {
  await page.goto(path);
  await expect(page).toHaveURL(/\/sign-in/);
  await page.getByLabel("Username").fill(who.user);
  await page.getByLabel("Password").fill(who.password);
  await page.getByRole("button", { name: "Sign in" }).click();
  // Signed in only once it has left this page (a sign-in page has an h1 too).
  await expect(page).not.toHaveURL(/\/sign-in/);
  await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
}

/** Runs `action` and waits for the change it makes to be saved on the server (a page change right after would cancel it). */
async function saved(page: Page, action: () => Promise<unknown>) {
  await Promise.all([
    page.waitForResponse((r) => r.url().endsWith("/api/v1/me/settings") && r.request().method() === "PUT"),
    action(),
  ]);
}

/** A browser of its own: cookies and storage apart from every other. */
async function elsewhere(browser: Browser) {
  const context = await browser.newContext({ viewport: { width: 1360, height: 900 } });
  return { context, page: await context.newPage() };
}

test("a wrong password is refused without saying which part was wrong", async ({ page }) => {
  await page.goto("/");
  await expect(page).toHaveURL(/\/sign-in/);
  await page.getByLabel("Username").fill(admin.user);
  await page.getByLabel("Password").fill("not the password");
  await page.getByRole("button", { name: "Sign in" }).click();
  const refused = page.getByRole("alert");
  await expect(refused).toHaveText("wrong username or password");

  await page.getByLabel("Username").fill("nobody-has-this-name");
  await page.getByLabel("Password").fill("not the password");
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(refused).toHaveText("wrong username or password");

  // Usernames ignore case.
  await page.getByLabel("Username").fill(admin.user.toUpperCase());
  await page.getByLabel("Password").fill(admin.password);
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page).not.toHaveURL(/\/sign-in/);
  await expect(page.getByRole("button", { name: new RegExp(`${admin.user}.*admin`) })).toBeVisible();
  // Neither the password nor anything like it is kept where page scripts can read it.
  const kept = await page.evaluate(() => JSON.stringify({ ...localStorage, ...sessionStorage }));
  expect(kept).not.toContain(admin.password);
});

test("without the payloads permission a message is shown without its content", async ({ page, browser }) => {
  await signInAs(page, viewer, "/interfaces/orders?tab=messages");
  await page.getByRole("button", { name: "quarantined" }).click();
  await page.getByRole("table", { name: "orders messages" }).getByRole("row").nth(1).getByRole("link").click();
  await expect(page.getByRole("heading", { level: 1 })).toContainText("quarantined");
  // The why is there; the patient's message is not.
  await expect(page.getByText("unknown patient").first()).toBeVisible();
  await expect(page.getByText("You don’t have permission to read payloads")).toBeVisible();
  await expect(page.locator("pre")).toHaveCount(0);
  await expect(page.locator("[data-path]")).toHaveCount(0);
  await page.screenshot({ path: "test-results/message-without-payloads.png", fullPage: true });
  const url = page.url();

  // The operator account was given the permission: the same message, with its content.
  const other = await elsewhere(browser);
  await signInAs(other.page, operator, new URL(url).pathname);
  await expect(other.page.getByText("You don’t have permission to read payloads")).toHaveCount(0);
  await other.page.getByRole("button", { name: "Raw" }).click();
  await expect(other.page.locator("pre")).toContainText("PID|1||");
  await other.context.close();
});

test("each role sees its own navigation and is told what it may not do", async ({ page, browser }) => {
  await signInAs(page, viewer);
  const nav = page.getByRole("navigation", { name: "Main" });
  await expect(nav.getByRole("link", { name: "Users" })).toHaveCount(0);
  await page.goto("/users");
  await expect(page.getByText("Managing users needs an admin.")).toBeVisible();
  await page.goto("/drain");
  await expect(page.getByRole("button", { name: "Start draining" })).toHaveCount(0);

  const other = await elsewhere(browser);
  await signInAs(other.page, admin);
  await expect(other.page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: "Users" })).toBeVisible();
  await other.page.goto("/users");
  const table = other.page.getByRole("table", { name: "Users" });
  await expect(table.getByRole("row", { name: new RegExp(viewer.user) })).toContainText("viewer");
  await expect(table.getByRole("row", { name: new RegExp(operator.user) })).toContainText("payloads");
  await other.page.screenshot({ path: "test-results/users.png", fullPage: true });
  await other.context.close();
});

test("an admin makes an account; its person must choose their own password before anything else", async ({
  page,
  browser,
}) => {
  const name = `${PREFIX}new-${Date.now()}`;
  await signInAs(page, admin, "/users");
  await page.getByRole("button", { name: "New user" }).click();
  const dialog = page.getByRole("dialog");
  await dialog.getByLabel("Username").fill(name);
  await dialog.getByLabel("Name", { exact: true }).fill("New Person");
  await dialog.getByLabel("Temporary password").fill("short");
  await dialog.getByRole("button", { name: "Create" }).click();
  await expect(dialog.getByRole("alert")).toContainText("at least 12");
  await dialog.getByLabel("Temporary password").fill("temporary pass phrase");
  await dialog.getByRole("radio", { name: /operator/ }).check();
  await dialog.getByRole("button", { name: "Create" }).click();
  const row = page.getByRole("table", { name: "Users" }).getByRole("row", { name: new RegExp(name) });
  await expect(row).toContainText("Must change password");

  const fresh = await elsewhere(browser);
  const p = fresh.page;
  await signInAs(p, { user: name, password: "temporary pass phrase" });
  // Everything leads to the account page until the password is changed.
  await expect(p).toHaveURL(/\/account$/);
  await expect(p.getByText("Choose your own to continue")).toBeVisible();
  await p.goto("/interfaces");
  await expect(p).toHaveURL(/\/account$/);
  await p.getByLabel("Current password").fill("temporary pass phrase");
  await p.getByLabel("New password", { exact: true }).fill("a brand new phrase");
  await p.getByLabel("New password again").fill("a different phrase");
  await expect(p.getByText("The two don’t match.")).toBeVisible();
  await expect(p.getByRole("button", { name: "Change password" })).toBeDisabled();
  await p.getByLabel("New password again").fill("a brand new phrase");
  await p.getByRole("button", { name: "Change password" }).click();
  await expect(p).toHaveURL(/\/$/);
  await expect(p.getByRole("heading", { level: 1 })).toHaveText("Overview");

  // Their own account: details, what they may do, sessions, tokens.
  await p.goto("/account");
  await expect(p.getByRole("region", { name: "What you can do" })).toContainText("payloads: no, you may not");
  const tokens = p.getByRole("region", { name: "Your tokens" });
  await tokens.getByRole("button", { name: "New token" }).click();
  const make = p.getByRole("dialog");
  await make.getByLabel("Name").fill("deploy");
  // An operator can't hand a token more than they have.
  await expect(make.getByRole("radio", { name: /admin/ })).toHaveCount(0);
  await make.getByRole("button", { name: "Create" }).click();
  const secret = await p.getByRole("dialog").getByLabel("Token").inputValue();
  expect(secret).toMatch(/^dux_[0-9a-f]{64}$/);
  const answer = await p.request.get("/api/v1/interfaces", { headers: { Authorization: `Bearer ${secret}` } });
  expect(answer.status()).toBe(200);
  await p.getByRole("button", { name: "Done" }).click();
  await expect(tokens.getByRole("table")).toContainText(`${name}/deploy`);
  await p.screenshot({ path: "test-results/account.png", fullPage: true });

  // Disabling ends their session at once, and their token with it.
  await page.getByRole("button", { name: `Disable ${name}` }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Disable" }).click();
  await expect(row).toContainText("Disabled");
  const after = await p.request.get("/api/v1/interfaces", { headers: { Authorization: `Bearer ${secret}` } });
  expect(after.status()).toBe(401);
  await p.reload();
  await expect(p).toHaveURL(/\/sign-in/);
  await fresh.context.close();
});

test("the theme and colors follow the person to another browser", async ({ browser }) => {
  const first = await elsewhere(browser);
  await signInAs(first.page, operator);
  await first.page.getByRole("button", { name: new RegExp(operator.user) }).click();
  await saved(first.page, () => first.page.getByRole("menuitemradio", { name: "High contrast" }).click());
  await expect(first.page.locator("html")).toHaveAttribute("data-theme", "contrast");

  // A custom scheme, too: the accent the other browser must wear.
  await first.page.goto("/appearance");
  await first.page.getByLabel("Accent as hex").fill("#0b5e2f");
  await saved(first.page, () => first.page.getByRole("button", { name: "Use these colors" }).click());
  await expect(first.page.locator("html")).toHaveAttribute("data-theme", "custom");

  const second = await elsewhere(browser);
  await signInAs(second.page, operator);
  await expect(second.page.locator("html")).toHaveAttribute("data-theme", "custom");
  const worn = await second.page.evaluate(() => JSON.parse(localStorage.getItem("ductus.colors") ?? "{}"));
  expect(worn.colors.accent).toBe("#0b5e2f");

  // Another person in a browser of their own has the default.
  const third = await elsewhere(browser);
  await signInAs(third.page, viewer);
  expect(await third.page.evaluate(() => document.documentElement.dataset.theme)).toBeUndefined();

  // Signing out leaves the next person here with the default, not this person's colors.
  await second.page.getByRole("button", { name: new RegExp(operator.user) }).click();
  await second.page.getByRole("menuitem", { name: "Sign out" }).click();
  await expect(second.page).toHaveURL(/\/sign-in/);
  expect(await second.page.evaluate(() => document.documentElement.dataset.theme)).toBeUndefined();
  expect(await second.page.evaluate(() => localStorage.getItem("ductus.colors"))).toBeNull();

  // Put the operator account back to the default for whoever runs the demo next.
  await first.page.goto("/appearance");
  await saved(first.page, () => first.page.getByRole("button", { name: "Reset" }).click());
  await first.page.getByRole("button", { name: new RegExp(operator.user) }).click();
  await saved(first.page, () => first.page.getByRole("menuitemradio", { name: "System" }).click());
  for (const c of [first, second, third]) await c.context.close();
});

test("too many wrong passwords lock the account until an admin unlocks it", async ({ page, browser }) => {
  const name = `${PREFIX}lock-${Date.now()}`;
  const api = (await browser.newContext()).request;
  await signInAs(page, admin, "/users");
  await page.getByRole("button", { name: "New user" }).click();
  const dialog = page.getByRole("dialog");
  await dialog.getByLabel("Username").fill(name);
  await dialog.getByLabel("Temporary password").fill("temporary pass phrase");
  await dialog.getByRole("button", { name: "Create" }).click();
  const row = page.getByRole("table", { name: "Users" }).getByRole("row", { name: new RegExp(name) });
  await expect(row).toBeVisible();

  const attempt = (password: string) =>
    api.post("/api/v1/session", { headers: { "X-Ductus": "1" }, data: { username: name, password } });
  for (let n = 0; n < 5; n++) expect((await attempt("wrong password!")).status()).toBe(401);
  const locked = await attempt("temporary pass phrase");
  expect(locked.status()).toBe(429);
  expect(await locked.text()).toContain("try again in");

  await page.reload();
  await expect(row).toContainText("Locked until");
  await row.getByRole("button", { name: `Unlock ${name}` }).click();
  await expect(row).not.toContainText("Locked until");
  expect((await attempt("temporary pass phrase")).status()).toBe(200);
});

const AA_TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"];

/** The account screens carry the most controls; axe visits them, signed in as an admin, in every theme. */
test("no accessibility violations on the account screens in any theme", async ({ page }) => {
  test.setTimeout(240_000);
  await signInAs(page, admin);
  const pages = ["/account", "/users", "/tokens"];
  const themes = ["light", "dark", "contrast"];
  for (const t of themes) {
    // On the server as well as here: a reload applies the account's choice.
    await page.request.put("/api/v1/me/settings", { headers: { "X-Ductus": "1" }, data: { theme: t } });
    await page.evaluate((name) => localStorage.setItem("ductus.theme", name), t);
    for (const path of pages) {
      await page.goto(path);
      await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
      await expect(page.getByText("Loading…")).toHaveCount(0);
      const tags = t === "contrast" ? [...AA_TAGS, "wcag2aaa"] : AA_TAGS;
      const { violations } = await new AxeBuilder({ page }).withTags(tags).analyze();
      const found = violations.map((v) => `${v.id}: ${v.nodes.map((n) => n.target.join(" ")).join(", ")}`);
      expect(found, `${t}, ${path}`).toEqual([]);
    }
  }
  // The dialogs open on top of them.
  await page.goto("/users");
  await page.getByRole("button", { name: "New user" }).click();
  const { violations } = await new AxeBuilder({ page }).withTags([...AA_TAGS, "wcag2aaa"]).analyze();
  expect(violations.map((v) => v.id)).toEqual([]);
});

test("the account screens fit a phone", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await signInAs(page, admin);
  for (const path of ["/account", "/users", "/tokens"]) {
    await page.goto(path);
    await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
    const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
    expect(overflow, path).toBeLessThanOrEqual(0);
  }
});

test("the sign-in page fits a phone, and its two forms pass axe", async ({ browser }) => {
  const context = await browser.newContext({ viewport: { width: 390, height: 844 } });
  const page = await context.newPage();
  // The default themes promise AA; High contrast promises AAA.
  for (const theme of ["light", "dark", "contrast"]) {
    await page.goto("/sign-in");
    await page.evaluate((t) => localStorage.setItem("ductus.theme", t), theme);
    await page.reload();
    for (const form of ["password", "token"]) {
      if (form === "token") await page.getByRole("button", { name: "Use an API token instead" }).click();
      const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
      expect(overflow, form).toBeLessThanOrEqual(0);
      const tags = theme === "contrast" ? [...AA_TAGS, "wcag2aaa"] : AA_TAGS;
      const { violations } = await new AxeBuilder({ page }).withTags(tags).analyze();
      expect(
        violations.map((v) => v.id),
        `${theme}, ${form}`,
      ).toEqual([]);
    }
  }
  await context.close();
});
