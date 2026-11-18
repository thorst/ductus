// The Releases page (0.7.4.5) against an engine in release mode that scripts/demo-ui.sh starts
// beside the demo's (DUCTUS_RELEASES_URL: project `ui-releases`, environment `staging`, only
// releases made on main, pulling from the demo's engine). Release 1 runs; release 2 (a file, only
// its code changed) is uploaded, checked and deployed here in place (0.7.5.1: the page says so
// first); release 3, made on a feature branch, is refused by the rule; then staging is rolled
// back to release 1, in place too. Release 4 adds an interface (0.7.5.2.1): deployed in place, then
// rolled back (the interface removed in place). Release 5 moves the interface's port (0.7.5.2.2):
// its source changed in place; release 6 moves it again by its variable's value alone (0.7.5.2.3:
// the version the same, the values in its history). A viewer sees it all and can change nothing.
// Runs last (playwright.config.ts): it deploys.

import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page } from "@playwright/test";

const base = process.env.DUCTUS_RELEASES_URL ?? "http://127.0.0.1:7795";
const deployer = process.env.DUCTUS_DEPLOYER_TOKEN ?? "";
const viewer = process.env.DUCTUS_VIEWER_TOKEN ?? "";
const file = process.env.DUCTUS_RELEASE_FILE ?? "";
const [r1 = "", r2 = "", r3 = "", r4 = "", r5 = "", r6 = "", r7 = ""] = (process.env.DUCTUS_RELEASES ?? "").split(" ");

async function signIn(page: Page, path: string, token = deployer) {
  await page.goto(`${base}${path}`);
  await page.getByRole("button", { name: "Use an API token instead" }).click();
  await page.getByLabel("Token").fill(token);
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page).not.toHaveURL(/\/sign-in/);
  await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
  await expect(page.getByText("Loading…")).toHaveCount(0);
}

async function axe(page: Page, what: string, include?: string) {
  let builder = new AxeBuilder({ page }).withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"]);
  if (include) builder = builder.include(include);
  const { violations } = await builder.analyze();
  const found = violations.map((v) => `${v.id}: ${v.nodes.map((n) => n.target.join(" ")).join(", ")}`);
  expect(found, what).toEqual([]);
}

// A check, then a deploy's handover: seconds each, more than the default 30 s together on a busy CI.
test.describe.configure({ mode: "serial", timeout: 120_000 });

test("the page says what this engine runs and this environment's rules", async ({ page }) => {
  await signIn(page, "/releases");
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Releases");
  await expect(page.getByText(/This engine runs releases of ui-releases in/)).toContainText(`release ${r1} now`);
  await expect(page.getByText("staging takes only releases made on main.")).toBeVisible();
  const stored = page.getByRole("table", { name: "Releases stored here" });
  await expect(stored.getByRole("row", { name: new RegExp(r1) })).toContainText("Running here");
  await expect(stored.getByRole("row", { name: new RegExp(r3) })).toContainText("feature/x");
  // The engine it pulls from (the demo's, `dev`, on the same database): its releases are here already.
  const upstream = page.getByRole("table", { name: "Releases on dev" });
  await expect(upstream.getByRole("row", { name: new RegExp(r1) })).toContainText("Stored here");
  await axe(page, "/releases");
  await page.screenshot({ path: "test-results/releases.png", fullPage: true });
});

test("a release is uploaded, checked first, and deployed reusing that check", async ({ page }) => {
  await signIn(page, "/releases");
  await page.getByLabel("Release file").setInputFiles(file);
  await expect(page.getByText(`Stored release ${r2}.`)).toBeVisible();
  await page.getByRole("table", { name: "Releases stored here" }).getByRole("link", { name: r2 }).click();
  await expect(page.getByRole("heading", { level: 1 })).toContainText(`Release ${r2}`);
  await expect(page.getByText("1 interface changes; 0 as they run now.")).toBeVisible();
  // Only its code changes (0.7.5.1): it applies in place, and the interface's line says so.
  await expect(page.getByTestId("applies")).toHaveText(
    "It applies in place: the interface it changes takes its new code between two messages; the others never pause.",
  );
  const changes = page.getByRole("table", { name: "What it changes in staging" });
  await expect(changes.getByRole("columnheader", { name: "Applies" })).toBeVisible();
  await expect(changes.locator("tbody tr").first()).toContainText("changes: code");
  await expect(changes.locator("tbody tr").first()).toContainText("in place");
  await axe(page, "a release's page");

  await page.getByRole("button", { name: "Deploy to staging…" }).click();
  const dialog = page.getByRole("dialog", { name: `Deploy release ${r2} to staging` });
  await expect(dialog).toContainText("staging takes only releases made on main.");
  await expect(dialog).toContainText("It applies in place");
  await axe(page, "the deploy dialog", "[role=dialog]");
  await dialog.getByRole("button", { name: "Check first" }).click();
  await expect(dialog.getByText(/^Passed$/)).toBeVisible({ timeout: 60_000 });
  await dialog.getByLabel(/Reason/).fill("archive: TWO");
  await dialog.getByRole("button", { name: "Deploy", exact: true }).click();
  await expect(dialog.getByRole("status")).toContainText(
    "applied in place: 1 interface took its new code between two messages; the others never paused",
    { timeout: 60_000 },
  );
  await expect(dialog).toContainText("The gate: the check from");
  await page.screenshot({ path: "test-results/release-deployed.png", fullPage: true });
  await dialog.getByRole("button", { name: "Done" }).click();
  await expect(page.getByRole("heading", { level: 1 })).toContainText("Running here");
});

test("a release made on another branch is refused by this environment's rule, and can't be forced", async ({
  page,
}) => {
  await signIn(page, `/releases/${r3}`);
  await page.getByRole("button", { name: "Deploy to staging…" }).click();
  const dialog = page.getByRole("dialog", { name: `Deploy release ${r3} to staging` });
  await dialog.getByLabel(/Reason/).fill("just this once");
  await dialog.getByRole("button", { name: "Deploy", exact: true }).click();
  const refused = dialog.getByRole("alert");
  await expect(refused).toContainText("refused by this environment's rules", { timeout: 60_000 });
  await expect(refused).toContainText("made on `feature/x`: merge it, then release from there");
  await expect(dialog.getByLabel(/Deploy over their work/)).toHaveCount(0);
  await axe(page, "a refusal", "[role=dialog]");
});

test("staging is rolled back to the release that ran before", async ({ page }) => {
  await signIn(page, "/releases");
  const ran = page.getByRole("table", { name: "What ran in staging" });
  await expect(ran.locator("tbody tr").first()).toContainText(r2);
  await expect(ran.locator("tbody tr").first()).toContainText("Running");
  await ran
    .getByRole("row", { name: new RegExp(r1) })
    .getByRole("button", { name: "Roll back to it…" })
    .click();
  const dialog = page.getByRole("dialog", { name: `Roll staging back to release ${r1}` });
  await expect(dialog.getByRole("button", { name: "Check first" })).toHaveCount(0);
  await dialog.getByLabel(/Reason/).fill("TWO broke the archive");
  await dialog.getByRole("button", { name: "Roll back" }).click();
  await expect(dialog.getByRole("status")).toContainText("applied in place", { timeout: 60_000 });
  await dialog.getByRole("button", { name: "Done" }).click();
  const first = ran.locator("tbody tr").first();
  await expect(first).toContainText(r1);
  await expect(first).toContainText("Rollback");
  await expect(first).toContainText("TWO broke the archive");
});

test("a release adding an interface goes in place, and a rollback removes it in place", async ({ page }) => {
  const added = `${process.env.DUCTUS_RELEASE_INTERFACE ?? ""}-copies`;
  await signIn(page, `/releases/${r4}`);
  await expect(page.getByText("1 interface changes, 1 interface new here; 0 as they run now.")).toBeVisible();
  await expect(page.getByTestId("applies")).toHaveText(
    "It applies in place: the interface it changes takes its new code between two messages, the one it adds starts; the others never pause.",
  );
  const changes = page.getByRole("table", { name: "What it changes in staging" });
  const line = changes.getByRole("row", { name: new RegExp(added) });
  await expect(line).toContainText("new here");
  await expect(line).toContainText("in place");
  await page.getByRole("button", { name: "Deploy to staging…" }).click();
  const dialog = page.getByRole("dialog", { name: `Deploy release ${r4} to staging` });
  await dialog.getByLabel(/Reason/).fill("copies of the archive");
  await dialog.getByRole("button", { name: "Deploy", exact: true }).click();
  await expect(dialog.getByRole("status")).toContainText("applied in place", { timeout: 60_000 });
  await dialog.getByRole("button", { name: "Done" }).click();
  await expect(page.getByRole("heading", { level: 1 })).toContainText("Running here");

  await page.goto(`${base}/releases`);
  const ran = page.getByRole("table", { name: "What ran in staging" });
  await expect(ran.locator("tbody tr").first()).toContainText(r4);
  await ran
    .getByRole("row", { name: new RegExp(r1) })
    .first()
    .getByRole("button", { name: "Roll back to it…" })
    .click();
  const back = page.getByRole("dialog", { name: `Roll staging back to release ${r1}` });
  await back.getByLabel(/Reason/).fill("no copies after all");
  await back.getByRole("button", { name: "Roll back" }).click();
  await expect(back.getByRole("status")).toContainText("applied in place", { timeout: 60_000 });
  await back.getByRole("button", { name: "Done" }).click();
  await expect(ran.locator("tbody tr").first()).toContainText("no copies after all");
});

test("a release moving an interface's port changes its source in place", async ({ page }) => {
  const name = process.env.DUCTUS_RELEASE_INTERFACE ?? "";
  await signIn(page, `/releases/${r5}`);
  await expect(page.getByText("1 interface changes; 0 as they run now.")).toBeVisible();
  await expect(page.getByTestId("applies")).toHaveText(
    "It applies in place: the source it changes moves, swaps its settings or restarts alone; the others never pause.",
  );
  const line = page
    .getByRole("table", { name: "What it changes in staging" })
    .getByRole("row", { name: new RegExp(name) });
  await expect(line).toContainText("changes: source");
  await expect(line).toContainText("in place");
  await page.getByRole("button", { name: "Deploy to staging…" }).click();
  const dialog = page.getByRole("dialog", { name: `Deploy release ${r5} to staging` });
  await dialog.getByLabel(/Reason/).fill("the archive on 6728");
  await dialog.getByRole("button", { name: "Deploy", exact: true }).click();
  await expect(dialog.getByRole("status")).toContainText(
    "applied in place: 1 interface's source changed; the others never paused",
    { timeout: 60_000 },
  );
  await dialog.getByRole("button", { name: "Done" }).click();
  await expect(page.getByRole("heading", { level: 1 })).toContainText("Running here");
});

test("a release changing only a variable's value moves the port in place, the version the same", async ({ page }) => {
  const name = process.env.DUCTUS_RELEASE_INTERFACE ?? "";
  await signIn(page, `/releases/${r6}`);
  await expect(page.getByText("1 interface takes new variable values; 0 as they run now.")).toBeVisible();
  await expect(page.getByTestId("project-changes")).toContainText(
    'environments/staging.yaml: ARCHIVE_PORT "6728" → "6729"',
  );
  await expect(page.getByTestId("applies")).toHaveText(
    "It applies in place: the source it changes moves, swaps its settings or restarts alone; the others never pause.",
  );
  const line = page
    .getByRole("table", { name: "What it changes in staging" })
    .getByRole("row", { name: new RegExp(name) });
  await expect(line).toContainText("new values: source (ARCHIVE_PORT)");
  await expect(line).toContainText("the same");
  await expect(line).toContainText("in place");
  await page.getByRole("button", { name: "Deploy to staging…" }).click();
  const dialog = page.getByRole("dialog", { name: `Deploy release ${r6} to staging` });
  await dialog.getByLabel(/Reason/).fill("the archive on 6729");
  await dialog.getByRole("button", { name: "Deploy", exact: true }).click();
  await expect(dialog.getByRole("status")).toContainText(
    "applied in place: 1 interface's source changed; the others never paused",
    { timeout: 60_000 },
  );
  await dialog.getByRole("button", { name: "Done" }).click();
  await expect(page.getByRole("heading", { level: 1 })).toContainText("Running here");

  // The interface's history has the values.
  await page.goto(`${base}/interfaces/${name}`);
  await page.getByRole("tab", { name: "History" }).click();
  await expect(
    page.getByText('deployed new values: ARCHIVE_PORT "6728" → "6729" in place: the archive on 6729'),
  ).toBeVisible();
});

test("a release changing what a start sets up restarts the interface alone, in place", async ({ page }) => {
  const name = process.env.DUCTUS_RELEASE_INTERFACE ?? "";
  await signIn(page, `/releases/${r7}`);
  await expect(page.getByTestId("applies")).toHaveText(
    "It applies in place: the interface whose settings change restarts alone; the others never pause.",
  );
  const line = page
    .getByRole("table", { name: "What it changes in staging" })
    .getByRole("row", { name: new RegExp(name) });
  await expect(line).toContainText("changes: dedupe");
  await expect(line).toContainText("in place, restarting alone: `dedupe` changed");
  await page.getByRole("button", { name: "Deploy to staging…" }).click();
  const dialog = page.getByRole("dialog", { name: `Deploy release ${r7} to staging` });
  await dialog.getByLabel(/Reason/).fill("no repeats");
  await dialog.getByRole("button", { name: "Deploy", exact: true }).click();
  await expect(dialog.getByRole("status")).toContainText(
    "applied in place: 1 interface was restarted alone on new settings; the others never paused",
    { timeout: 60_000 },
  );
  await dialog.getByRole("button", { name: "Done" }).click();
  await expect(page.getByRole("heading", { level: 1 })).toContainText("Running here");
});

test("a viewer sees the releases and changes nothing", async ({ page }) => {
  await signIn(page, "/releases", viewer);
  await expect(page.getByRole("table", { name: "Releases stored here" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Upload a release" })).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Roll back to it…" })).toHaveCount(0);
  await page.goto(`${base}/releases/${r2}`);
  await expect(page.getByRole("heading", { level: 1 })).toContainText(`Release ${r2}`);
  await expect(page.getByRole("button", { name: /Deploy to/ })).toHaveCount(0);
  await expect(page.getByRole("link", { name: "Download" })).toHaveCount(0);
});
