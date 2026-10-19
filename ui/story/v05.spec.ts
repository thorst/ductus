// The v0.5 story: one on-call night, in the web UI (scripts/demo-v05.sh sets the scene and checks
// the mail and the audit log afterwards). Olaf, an operator from the directory, owns adt-lab.
//   1. Olaf signs in with his directory password; his Interfaces page opens on his own.
//   2. The lab's receiver rejects one patient's messages: three are quarantined.
//   3. The receiver goes down. The destination stalls; the page says why (connection refused);
//      the alert is mailed to on-call and to Olaf. The lab portal's certificate, 7 days from
//      expiry, was alerted by the engine's own check meanwhile.
//   4. Olaf pauses the destination with a note, the receiver comes back, he resumes it: the
//      backlog goes and the recovery is mailed.
//   5. The three quarantined messages: one resent as it was (the lab registered the patient), one
//      edited (a wrong MRN), one dismissed (a training message).
//   6. Ada (admin, auditor) reads the audit log: everything Olaf did, including reading payloads.

import { spawn, spawnSync, type ChildProcess } from "node:child_process";
import { openSync, readFileSync } from "node:fs";
import { expect, test, type Page } from "@playwright/test";

function person(key: string) {
  const [user = "", ...rest] = (process.env[key] ?? "").split(":");
  return { user, password: rest.join(":") };
}

const olaf = person("DUCTUS_DIRECTORY_OPERATOR");
const ada = person("DUCTUS_DIRECTORY_AUDITOR");
const BIN = process.env.DUCTUS_BIN ?? "ductus";
const SOURCE = process.env.DUCTUS_STORY_SOURCE ?? "127.0.0.1:6751";
const LAB_PORT = process.env.DUCTUS_STORY_LAB_PORT ?? "6752";
const MAIL = process.env.DUCTUS_STORY_MAIL ?? "mail.log";
const LOGS = process.env.DUCTUS_STORY_LOGS ?? ".";
/** The patient whose messages the lab can't take at first (MRN 100001; `ductus gen` numbers from 100000). */
const REJECTED = "||100001^^^MRN";

test.describe.configure({ mode: "serial" });

let page: Page;
let receiver: ChildProcess | undefined;
// Filled in step 5, read in step 6.
const ids = { resent: 0, edited: 0, dismissed: 0 };

function startReceiver(...args: string[]) {
  const log = openSync(`${LOGS}/lab.log`, "a");
  receiver = spawn(BIN, ["sink", "--port", LAB_PORT, ...args], { stdio: ["ignore", log, log] });
}

function stopReceiver() {
  receiver?.kill("SIGINT");
  receiver = undefined;
}

function send(count: number) {
  const r = spawnSync(BIN, ["gen", "--to", SOURCE, "--count", String(count), "--patients", "3"], { encoding: "utf8" });
  expect(r.status, r.stderr).toBe(0);
}

/** The mail sink's log: one line per mail, `mail N from X to Y: subject`. */
function mails(): string[] {
  try {
    return readFileSync(MAIL, "utf8")
      .split("\n")
      .filter((l) => l.startsWith("mail "));
  } catch {
    return [];
  }
}

async function signIn(who: { user: string; password: string }, path = "/") {
  await page.goto(path);
  await expect(page).toHaveURL(/\/sign-in/);
  await page.getByLabel("Username").fill(who.user);
  await page.getByLabel("Password").fill(who.password);
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page).not.toHaveURL(/\/sign-in/);
  await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
}

const quarantined = () => page.getByRole("table", { name: "Quarantined deliveries" }).locator("tbody tr");

test.beforeAll(async ({ browser }) => {
  page = await (await browser.newContext()).newPage();
});

test.afterAll(async () => {
  stopReceiver();
  await page.context().close();
});

test("1. an operator signs in through the directory and sees his interfaces", async () => {
  await signIn(olaf, "/interfaces");
  await expect(page.getByRole("button", { name: new RegExp(`${olaf.user}.*operator`) })).toBeVisible();
  await expect(page.getByRole("button", { name: "Mine (1)" })).toHaveAttribute("aria-pressed", "true");
  await expect(page.getByRole("table").getByRole("link", { name: "adt-lab", exact: true })).toBeVisible();
  await page.screenshot({ path: "test-results/story/1-mine.png", fullPage: true });
});

test("2. the lab rejects one patient's messages: they are quarantined", async () => {
  startReceiver("--reject", REJECTED);
  send(9);
  await page.goto("/quarantine");
  await expect(quarantined()).toHaveCount(3, { timeout: 20_000 });
  await expect(quarantined().first()).toContainText("rejected (AR): rejected by the receiver");
});

test("3. the receiver goes down: stalled, diagnosed, mailed; the certificate was alerted too", async () => {
  test.setTimeout(240_000);
  stopReceiver();
  send(5);
  await page.goto("/interfaces/adt-lab");
  const lab = page.getByRole("region", { name: "lab", exact: true });
  // Stalled once nothing has been sent for a minute (and the engine has watched for one).
  await expect(lab).toContainText("Stalled", { timeout: 150_000 });
  await expect(lab).toContainText("nothing sent for");
  await expect(lab).toContainText("Connection refused");
  await expect(lab.getByRole("link", { name: "Where it's stuck" })).toBeVisible();
  await page.screenshot({ path: "test-results/story/3-stalled.png", fullPage: true });

  await expect
    .poll(() => mails().filter((m) => m.includes("CRITICAL adt-lab → lab is stalled")).length, { timeout: 60_000 })
    .toBe(2);
  expect(mails().some((m) => m.includes("to olaf@example.org: [Ductus dev] CRITICAL adt-lab → lab is stalled"))).toBe(
    true,
  );

  // The engine's own certificate check (a minute or two after it started) found the portal's.
  await page.goto("/alerts");
  const open = page.getByRole("region", { name: "Open alerts" });
  await expect(open.getByRole("listitem").filter({ hasText: "adt-lab → lab" })).toContainText("stalled");
  await expect(open).toContainText("expires in 7 days", { timeout: 90_000 });
  await expect.poll(() => mails().filter((m) => m.includes("expires in 7 days")).length, { timeout: 30_000 }).toBe(2);
  await page.screenshot({ path: "test-results/story/3-alerts.png", fullPage: true });
});

test("4. paused with a note; the receiver is back; resumed; the recovery is mailed", async () => {
  test.setTimeout(240_000);
  await page.goto("/interfaces/adt-lab");
  const lab = page.getByRole("region", { name: "lab", exact: true });
  await lab.getByRole("button", { name: "Pause lab" }).click();
  await page
    .getByRole("dialog")
    .getByLabel(/Reason/)
    .fill("lab vendor restarting their engine, ticket 4471");
  await page.getByRole("dialog").getByRole("button", { name: "Pause" }).click();
  await expect(lab).toContainText("Paused");
  await expect(lab).toContainText("lab vendor restarting their engine, ticket 4471");

  startReceiver();
  await lab.getByRole("button", { name: "Resume lab" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Resume" }).click();
  // The backlog goes (after the retry back-off it was in), then the recovery after `settle`.
  const waiting = async () => {
    const r = await page.request.get("/api/v1/interfaces/adt-lab");
    const i = (await r.json()) as { destinations: { name: string; pending: number; paused: unknown }[] };
    const d = i.destinations.find((x) => x.name === "lab");
    return d && !d.paused ? d.pending : -1;
  };
  await expect.poll(waiting, { timeout: 120_000 }).toBe(0);
  await expect
    .poll(() => mails().filter((m) => m.includes("RECOVERED adt-lab → lab")).length, { timeout: 90_000 })
    .toBe(2);
  await page.screenshot({ path: "test-results/story/4-recovered.png", fullPage: true });
});

test("5. the quarantine: one resent, one edited, one dismissed", async () => {
  await page.goto("/quarantine");
  await expect(quarantined()).toHaveCount(3);
  const id = async (n: number) => Number(await quarantined().nth(n).getByRole("link").first().innerText());
  [ids.resent, ids.edited, ids.dismissed] = [await id(0), await id(1), await id(2)];

  // The lab has registered the patient now: resent as it was (a resend of a quarantine, though the page names the destination).
  await page.goto(`/messages/${ids.resent}`);
  const destinations = page.getByRole("region", { name: "Destinations" });
  await destinations.getByRole("button", { name: "Resend" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Resend" }).click();
  await expect(page.getByRole("heading", { level: 1 })).toContainText("sent", { timeout: 20_000 });

  // A wrong MRN: corrected, and the copy goes.
  await page.goto(`/messages/${ids.edited}`);
  await page.getByRole("button", { name: "Edit and resend" }).click();
  const dialog = page.getByRole("dialog", { name: /Edit and resend/ });
  const editor = dialog.getByLabel(/Message/);
  await editor.fill((await editor.inputValue()).replace("100001^^^MRN", "100002^^^MRN"));
  await dialog.getByLabel(/Reason/).fill("registered under the wrong MRN; it is 100002");
  await dialog.getByRole("button", { name: "Queue the copy" }).click();
  await expect(page.getByRole("note")).toContainText("An edited copy of message");
  await expect(page.getByRole("heading", { level: 1 })).toContainText("sent", { timeout: 20_000 });

  // A training message that should never have reached the lab.
  await page.goto(`/messages/${ids.dismissed}`);
  await destinations.getByRole("button", { name: "Dismiss" }).click();
  await page.getByRole("dialog").getByLabel("Reason (required, kept in the audit log)").fill("ED training message");
  await page.getByRole("dialog").getByRole("button", { name: "Dismiss", exact: true }).click();
  await expect(destinations).toContainText(`dismissed by ${olaf.user}: ED training message`);

  await page.goto("/quarantine");
  await expect(quarantined()).toHaveCount(0);
  await page.screenshot({ path: "test-results/story/5-quarantine-empty.png", fullPage: true });
});

test("6. the audit log lists it all, including who read the payloads", async () => {
  await page.getByRole("button", { name: new RegExp(olaf.user) }).click();
  await page.getByRole("menuitem", { name: "Sign out" }).click();
  await signIn(ada, `/messages/${ids.edited}`);
  await expect(page.getByRole("region", { name: "Who viewed the payload" })).toContainText(olaf.user);

  await page.goto(`/audit?actor=${olaf.user}`);
  const table = page.getByRole("table", { name: "Audit log" });
  for (const what of [
    "Signed in",
    "Paused a destination",
    "Resumed a destination",
    "Viewed a payload",
    "Resent a quarantined message",
    "Edited and resent a message",
    "Dismissed a quarantined message",
  ]) {
    await expect(table.locator("tbody tr").filter({ hasText: what }).first(), what).toBeVisible();
  }
  for (const id of [ids.resent, ids.edited, ids.dismissed]) {
    await expect(table.getByRole("link", { name: `message ${id}`, exact: true }).first()).toBeVisible();
  }
  await expect(table).toContainText("ED training message");
  await expect(table).toContainText("lab vendor restarting their engine, ticket 4471");
  await page.screenshot({ path: "test-results/story/6-audit.png", fullPage: true });
});
