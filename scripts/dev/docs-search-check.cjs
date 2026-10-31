// Checks the docs' two searches in a real browser:
//   - the site (target/site, from docs-site.sh), served over HTTP: S opens Pagefind's search, a query
//     finds pages under their own titles, and ArrowDown + Enter opens the first one;
//   - the offline docs zip's pages (target/book, from docs-check.sh), opened from file://: mdBook's
//     search finds and opens pages, and none of the site's Pagefind search is on them.
// Exit 1 with what failed. Runs in docs.yml after both builds; by hand:
//
//   node scripts/dev/docs-search-check.cjs ui/node_modules/playwright
//
// The argument is where Playwright is (docs.yml installs playwright-core alone and sets
// DOCS_BROWSER=chrome, the runner's own Chrome, so no browser is downloaded).
"use strict";
const http = require("http");
const fs = require("fs");
const path = require("path");
const { chromium } = require(path.resolve(process.argv[2] || "ui/node_modules/playwright"));

const repo = path.resolve(__dirname, "../..");
const site = path.join(repo, "target/site");
const book = path.join(repo, "target/book");
const types = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".json": "application/json", ".wasm": "application/wasm", ".svg": "image/svg+xml", ".woff2": "font/woff2" };
const failures = [];
const check = (ok, what) => {
  console.log(`${ok ? "ok" : "FAILED"}: ${what}`);
  if (!ok) failures.push(what);
};

const server = http.createServer((req, res) => {
  let p = path.join(site, decodeURIComponent(req.url.split("?")[0]));
  if (p.endsWith("/")) p += "index.html";
  if (!p.startsWith(site) || !fs.existsSync(p) || fs.statSync(p).isDirectory()) { res.writeHead(404); return res.end(); }
  res.writeHead(200, { "Content-Type": types[path.extname(p)] || "application/octet-stream" });
  res.end(fs.readFileSync(p));
});

(async () => {
  for (const [dir, made] of [[site, "docs-site.sh"], [book, "docs-check.sh"]]) {
    if (!fs.existsSync(path.join(dir, "index.html"))) throw new Error(`${dir} isn't built: run scripts/dev/${made} first`);
  }
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  const base = `http://127.0.0.1:${server.address().port}`;
  const browser = await chromium.launch(process.env.DOCS_BROWSER ? { channel: process.env.DOCS_BROWSER } : {});
  const errors = [];
  const page = await browser.newPage();
  page.on("pageerror", (e) => errors.push(e.message));
  page.on("console", (m) => { if (m.type() === "error") errors.push(m.text()); });

  // The site: Pagefind.
  await page.goto(`${base}/operate/retention.html`, { waitUntil: "load" });
  const trigger = page.locator("#menu-bar pagefind-modal-trigger button");
  await trigger.waitFor({ timeout: 10000 }).catch(() => {});
  check((await trigger.count()) === 1, "the site's menu bar has Pagefind's search button");
  check((await trigger.getAttribute("aria-label").catch(() => null)) === "Search the docs", "the button is named for screen readers");
  check((await page.locator("#searchbar").count()) === 0, "mdBook's own search isn't on the site");
  await page.keyboard.press("s");
  const input = page.locator("pagefind-modal input").first();
  await input.waitFor({ timeout: 5000 }).catch(() => {});
  const opened = await input.isVisible();
  check(opened, "S opens the search");
  const results = page.locator("pagefind-modal pagefind-results a");
  let count = 0;
  if (opened) {
    await input.fill("handover");
    await results.first().waitFor({ timeout: 10000 }).catch(() => {});
    count = await results.count();
    check(count > 0, `"handover" finds pages (${count} links)`);
  }
  if (count > 0) {
    const title = (await results.first().textContent()).trim();
    const href = await results.first().getAttribute("href");
    check(title !== "" && title !== "Ductus", `results carry the page's own title ("${title}")`);
    await page.keyboard.press("ArrowDown");
    await page.keyboard.press("Enter");
    await page.waitForURL((u) => !u.pathname.endsWith("/operate/retention.html"), { timeout: 5000 }).catch(() => {});
    const at = page.url().replace(base, "");
    check(at === href, `ArrowDown + Enter opens the first result (${href}; at ${at})`);
  }
  // Ctrl+K (Pagefind's own shortcut) opens it too, on the page the result opened.
  if (opened) {
    await trigger.waitFor({ timeout: 10000 }).catch(() => {});
    await page.keyboard.press("Control+k");
    await page.waitForTimeout(300);
    check(await input.isVisible(), "Ctrl+K opens the search");
  }
  check(errors.length === 0, `no errors on the site's pages${errors.length ? `: ${errors.join(" | ")}` : ""}`);

  // The offline docs: mdBook's search from file://.
  errors.length = 0;
  await page.goto(`file://${book}/operate/retention.html`);
  await page.keyboard.press("s");
  await page.locator("#searchbar").fill("handover");
  const found = page.locator("#searchresults li a");
  await found.first().waitFor({ timeout: 10000 }).catch(() => {});
  check((await found.count()) > 0, `the offline pages search from file:// (${await found.count()} results)`);
  check((await page.locator("pagefind-modal, pagefind-modal-trigger").count()) === 0, "the site's Pagefind search stays off the offline pages");
  if ((await found.count()) > 0) {
    await found.first().click();
    await page.waitForLoadState("load");
    check(page.url().includes("highlight=handover"), "a result opens its page");
  }
  check(errors.length === 0, `no errors on the offline pages${errors.length ? `: ${errors.join(" | ")}` : ""}`);

  await browser.close();
  server.close();
  if (failures.length) {
    console.error(`\n${failures.length} check(s) failed`);
    process.exit(1);
  }
  console.log("\nboth searches work");
})().catch((e) => {
  console.error(e);
  process.exit(1);
});
