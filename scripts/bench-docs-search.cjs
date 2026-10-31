// First search on the docs site, mdBook's search (target/book) vs Pagefind's modal (target/site).
// The server throttles, not DevTools: Pagefind 1.5 searches in a Web Worker, whose requests a page's
// DevTools session neither counts nor throttles. Every response waits the profile's latency, then
// shares one link paced at its bandwidth (16 KB slices), gzipped as GitHub Pages serves it.
// Times from pressing S on a loaded page to the first result shown, 3 runs per profile.
// Results and why it's measured this way: architecture.md, "The docs site". After docs-check.sh and docs-site.sh:
//   node scripts/bench-docs-search.cjs mdbook "$PWD/target/book" ui/node_modules/playwright
//   node scripts/bench-docs-search.cjs pagefind "$PWD/target/site" ui/node_modules/playwright log
// (log: every request of the first 4 Mbit/s and local runs, with when it was asked and finished)
const http = require("http");
const fs = require("fs");
const path = require("path");
const zlib = require("zlib");
const { chromium } = require(path.resolve(process.argv[4]));
const mode = process.argv[2];
const root = process.argv[3];
const logRequests = process.argv[5] === "log";
const types = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".json": "application/json", ".wasm": "application/wasm", ".svg": "image/svg+xml", ".woff2": "font/woff2" };
const cache = new Map();
let net = null; // { latency ms, bps bytes/s }
let nextFree = 0;
let log = null;
let t0 = 0;
let served = { bytes: 0, requests: 0 };
const server = http.createServer((req, res) => {
  let p = path.join(root, decodeURIComponent(req.url.split("?")[0]));
  if (p.endsWith("/")) p += "index.html";
  if (!fs.existsSync(p) || fs.statSync(p).isDirectory()) { res.writeHead(404); return res.end(); }
  const ext = path.extname(p);
  let body = fs.readFileSync(p);
  const headers = { "Content-Type": types[ext] || "application/octet-stream", "Cache-Control": "no-store" };
  if ([".html", ".js", ".css", ".json", ".svg"].includes(ext)) {
    if (!cache.has(p)) cache.set(p, zlib.gzipSync(body, { level: 6 }));
    body = cache.get(p);
    headers["Content-Encoding"] = "gzip";
  }
  headers["Content-Length"] = body.length;
  served.bytes += body.length;
  served.requests++;
  const asked = Date.now();
  const done = () => { if (log) log.push(`${String(asked - t0).padStart(6)} ms â†’ ${String(Date.now() - t0).padStart(6)} ms  ${String(body.length).padStart(8)} B  ${req.url.split("?")[0]}`); };
  if (!net) { res.writeHead(200, headers); res.end(body); return done(); }
  setTimeout(() => {
    res.writeHead(200, headers);
    const slice = 16384;
    let at = Math.max(Date.now(), nextFree);
    for (let i = 0; i < body.length; i += slice) {
      const piece = body.subarray(i, i + slice);
      at += (piece.length / net.bps) * 1000;
      const last = i + slice >= body.length;
      setTimeout(() => { res.write(piece); if (last) { res.end(); done(); } }, Math.max(0, at - Date.now()));
    }
    if (body.length === 0) { res.end(); done(); }
    nextFree = at;
  }, net.latency);
});
const profiles = [
  ["no throttling (local)", null],
  ["office line, 10 Mbit/s, 40 ms", { latency: 40, bps: 10e6 / 8 }],
  ["4G, 4 Mbit/s, 100 ms", { latency: 100, bps: 4e6 / 8 }],
  ["3G, 1.6 Mbit/s, 150 ms", { latency: 150, bps: 1.6e6 / 8 }],
];
const input = mode === "mdbook" ? "#searchbar" : "pagefind-modal input";
const result = mode === "mdbook" ? "#searchresults li a" : "pagefind-modal pagefind-results a";
(async () => {
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  const base = `http://127.0.0.1:${server.address().port}`;
  const browser = await chromium.launch();
  for (const [name, profile] of profiles) {
    const times = [];
    let load = null, search = null;
    for (let run = 0; run < 3; run++) {
      net = profile;
      nextFree = 0;
      const context = await browser.newContext();
      const page = await context.newPage();
      served = { bytes: 0, requests: 0 };
      await page.goto(`${base}/introduction.html`, { waitUntil: "load" });
      if (mode === "pagefind") await page.locator("pagefind-modal-trigger button").waitFor();
      await page.waitForTimeout(1000); // whatever the page fetches after load counts as the page's
      load = served;
      served = { bytes: 0, requests: 0 };
      log = logRequests && run === 0 && (name.startsWith("4G") || name.startsWith("no ")) ? [] : null;
      // Timed in the page: Playwright's waitFor re-checks at growing intervals (up to 500 ms), which
      // would round every time up. The key press and the first result are stamped by the page itself.
      await page.evaluate((sel) => {
        window.__first = null;
        new MutationObserver(() => {
          if (window.__first === null && document.querySelector(sel)) window.__first = performance.now();
        }).observe(document, { subtree: true, childList: true });
        document.addEventListener("keydown", () => { window.__pressed ??= performance.now(); }, { capture: true });
      }, result);
      t0 = Date.now();
      await page.keyboard.press("s");
      await page.locator(input).first().fill("retention");
      await page.locator(result).first().waitFor({ timeout: 120000 });
      times.push(Math.round(await page.evaluate(() => window.__first - window.__pressed)));
      await page.waitForTimeout(1500);
      search = served;
      if (log) console.log(`  requests after S (4G, first run; first result at ${times[0]} ms):\n    ${log.join("\n    ")}`);
      log = null;
      await context.close();
    }
    console.log(`${mode} ${name}: first search ${times.join(", ")} ms; page load ${load.requests} requests, ${load.bytes} B; search ${search.requests} requests, ${search.bytes} B`);
  }
  await browser.close();
  server.close();
})();
