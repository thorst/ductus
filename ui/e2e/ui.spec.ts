// The web UI against a real engine (scripts/demo-ui.sh sets it up: adt's lab behind, orders
// quarantining patient 100003, billing paused).

import { readFileSync } from "node:fs";
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
  // A viewer looks; acting is for operators.
  await expect(page.getByRole("button", { name: "Edit and resend" })).toHaveCount(0);
  await expect(page.getByRole("button", { name: /^Resend/ })).toHaveCount(0);
});

test("the quarantine across interfaces", async ({ page }) => {
  await signIn(page, "/quarantine");
  const rows = page.getByRole("table", { name: "Quarantined deliveries" }).locator("tbody tr");
  await expect(rows).toHaveCount(4);
  await expect(rows.first()).toContainText("unknown patient 100003");
  await expect(page.getByRole("link", { name: /Quarantine\s*4/ }).first()).toBeVisible();
  // The filter offers the interfaces with something quarantined.
  await page.getByLabel("Interface").selectOption("orders");
  await expect(page).toHaveURL(/interface=orders/);
  await expect(rows).toHaveCount(4);
  await page.goto("/quarantine?interface=adt");
  await expect(page.getByText("Nothing quarantined in adt.")).toBeVisible();
  await expect(page.getByRole("button", { name: /Resend/ })).toHaveCount(0); // a viewer
});

test("a behind destination links to where its messages are stuck", async ({ page }) => {
  await signIn(page, "/interfaces/adt");
  await expect(page.getByRole("region", { name: "archive" }).getByRole("link", { name: /stuck/ })).toHaveCount(0);
  await page.getByRole("region", { name: "lab", exact: true }).getByRole("link", { name: "Where it's stuck" }).click();
  await expect(page).toHaveURL(/\/hotspots\?interface=adt&destination=lab$/);
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Hotspots: adt → lab");
  const lab = page.getByRole("region", { name: "adt → lab" });
  await expect(lab).toContainText(/\d+ waiting in \d+ of \d+ lanes/);
  const lanes = lab.getByRole("table", { name: "Lanes waiting for lab" }).locator("tbody tr");
  await expect(lanes.first()).toBeVisible();
  // One lane's first message is the one the worker keeps retrying; the others wait behind it.
  const blocked = lanes.filter({ hasText: /refused/i });
  await expect(blocked).toHaveCount(1);
  await expect(lanes.first()).toContainText(/refused/i); // listed first
  await expect(lab).toContainText(/blocked by message \d+/);
  await page.screenshot({ path: "test-results/hotspots.png", fullPage: true });
  await blocked.getByRole("link").click();
  await expect(page.getByRole("heading", { level: 1 })).toContainText("pending");
});

test("which interfaces a vendor touches, and the names known when none", async ({ page }) => {
  await signIn(page, "/impact");
  await page.getByLabel("Look for").selectOption("vendor");
  // (A label wrapping a select includes its options' text: "Look for" holds "Vendor" too.)
  const vendor = page.getByRole("textbox", { name: "Vendor" });
  await vendor.fill("epic systems");
  await page.getByRole("button", { name: "Find" }).click();
  await expect(page).toHaveURL(/vendor=epic/);
  await expect(page.getByRole("status")).toContainText("1 of 3 interfaces touch");
  const hits = page.getByRole("table", { name: "Affected interfaces" }).locator("tbody tr");
  await expect(hits.first()).toContainText("adt");
  await expect(hits.first()).toContainText("ADT team");

  await vendor.fill("Nobody Inc");
  await page.getByRole("button", { name: "Find" }).click();
  await expect(page.getByRole("status")).toContainText("No interface touches");
  await page.getByRole("button", { name: "Epic Systems" }).click();
  await expect(hits.first()).toContainText("adt");
});

test("moving an owner's interfaces is previewed as a patch for the repository", async ({ page }) => {
  await signIn(page, "/owners");
  const owners = page.getByRole("region", { name: "Owners", exact: true });
  for (const owner of ["ADT team", "Finance", "Orders team"]) await expect(owners).toContainText(owner);

  const move = page.getByRole("region", { name: "Move ownership" });
  await move.getByRole("combobox").selectOption("ADT team");
  await move.getByRole("textbox", { name: "To" }).fill("Integration team");
  await move.getByRole("button", { name: "Preview" }).click();
  const preview = page.getByRole("region", { name: "Preview" });
  await expect(preview).toContainText("1 owner value in 1 file would change. Nothing has been written.");
  await expect(preview).toContainText("interfaces/adt/interface.yaml");
  await expect(preview).toContainText("+   owner: Integration team");
  await expect(preview).toContainText("ductus owners reassign --from 'ADT team' --to 'Integration team'");
  const [download] = await Promise.all([
    page.waitForEvent("download"),
    preview.getByRole("button", { name: "Download the patch" }).click(),
  ]);
  const patch = readFileSync((await download.path())!, "utf8");
  expect(patch).toContain("--- a/interfaces/adt/interface.yaml");
  expect(patch).toContain("\n+  owner: Integration team\n");
  await page.screenshot({ path: "test-results/owners.png", fullPage: true });
  // Nothing was written: the owners are as they were.
  await page.reload();
  await expect(owners).toContainText("ADT team");
});

test("retention, the shorter window waiting, drain and connection checks, as a viewer", async ({ page }) => {
  await signIn(page, "/retention");
  const billing = page.getByRole("table", { name: "Retention by interface" }).getByRole("row", { name: /billing/ });
  await expect(billing).toContainText("7w (49 days)");
  await expect(billing).toContainText("Shorter (7 days) waits for a confirmation");
  await expect(page.getByRole("status")).toContainText("billing has a shorter retention waiting");
  await expect(page.getByRole("region", { name: "Held area" })).toContainText("Nothing held.");
  await expect(page.getByRole("button", { name: /Confirm/ })).toHaveCount(0); // a viewer

  await page.goto("/drain");
  await expect(page.getByRole("status")).toContainText("Taking messages.");
  await expect(page.getByRole("button", { name: "Start draining" })).toHaveCount(0);

  await page.goto("/connections");
  await expect(page.getByText("Running the checks needs an operator token.")).toBeVisible();

  // Tokens are an admin's: not in a viewer's navigation, and the page says why.
  await expect(page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: "Tokens" })).toHaveCount(0);
  await page.goto("/tokens");
  await expect(page.getByText("Managing tokens needs an admin token.")).toBeVisible();
});

test("the API reference is in the binary and loads only when opened", async ({ page }) => {
  const chunks: string[] = [];
  page.on("request", (r) => {
    if (/\/assets\/api-reference-[^/]+\.js$/.test(r.url())) chunks.push(r.url());
  });
  await signIn(page);
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Overview");
  expect(chunks, "a first visit doesn't load the reference").toEqual([]);

  await page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: "API reference" }).click();
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("API reference");
  expect(chunks).toHaveLength(1);
  // Every endpoint, in the engine's own words, with who may call it and a curl example.
  const health = page.getByRole("region", { name: "GET /health" });
  await expect(health).toContainText("no token");
  await expect(health).toContainText(`curl -s '${new URL(page.url()).origin}/api/v1/health'`);
  await page.getByLabel("Find an endpoint").fill("resend");
  const resend = page.getByRole("region", { name: "POST /messages/{id}/resend" });
  await expect(resend).toContainText("operator");
  await expect(resend).toContainText("-X POST");
  await expect(page.getByRole("region", { name: "GET /health" })).toHaveCount(0);
  await page.screenshot({ path: "test-results/api-reference.png", fullPage: true });
  await page.getByLabel("Find an endpoint").fill("");
  await page
    .getByRole("region", { name: "POST /messages/{id}/resend" })
    .getByRole("link", { name: "ResentMessage" })
    .click();
  await expect(page.getByRole("region", { name: "ResentMessage", exact: true })).toBeInViewport();
});

test("no accessibility violations in any theme", async ({ page }) => {
  test.setTimeout(180_000);
  await signIn(page);
  const pages = [
    "/",
    "/interfaces",
    "/interfaces/adt",
    "/interfaces/orders?tab=messages",
    "/quarantine",
    "/hotspots",
    "/hotspots?interface=adt&destination=lab",
    "/impact?vendor=Epic%20Systems",
    "/owners",
    "/retention",
    "/drain",
    "/connections",
    "/tokens",
    "/api-reference",
  ];
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
  for (const path of [
    "/",
    "/interfaces",
    "/interfaces/adt",
    "/interfaces/orders?tab=messages",
    "/quarantine",
    "/hotspots",
    "/impact?vendor=Epic%20Systems",
    "/owners",
    "/retention",
    "/drain",
    "/api-reference",
  ]) {
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
