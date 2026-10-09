// The UI's size budget, gzipped as the engine serves it. A first visit downloads what index.html
// loads (its script, the modules it preloads, the stylesheets): that must stay under BUDGET_KB.
// Pages split out with a dynamic import() (the API reference) load only when opened; each such
// chunk has its own limit, so nothing grows unnoticed. Over either, the build fails.
import { readdirSync, readFileSync } from "node:fs";
import { gzipSync } from "node:zlib";

const BUDGET_KB = 300;
const CHUNK_KB = 50;
const dist = new URL("../dist/", import.meta.url);
const html = readFileSync(new URL("index.html", dist), "utf8");
const first = new Set([...html.matchAll(/(?:src|href)="\/?assets\/([^"]+\.(?:js|css))"/g)].map((m) => m[1]));
const kb = (f) => gzipSync(readFileSync(new URL(`assets/${f}`, dist)), { level: 9 }).length / 1024;
const files = readdirSync(new URL("assets/", dist)).filter((f) => /\.(js|css)$/.test(f));

let total = 0;
let failed = false;
for (const f of files.filter((f) => first.has(f))) {
  const size = kb(f);
  total += size;
  console.log(`  ${f}  ${size.toFixed(1)} KB gzipped (first visit)`);
}
for (const f of files.filter((f) => !first.has(f))) {
  const size = kb(f);
  console.log(`  ${f}  ${size.toFixed(1)} KB gzipped (loaded when its page opens; limit ${CHUNK_KB} KB)`);
  if (size > CHUNK_KB) {
    console.error(`${f} is over the ${CHUNK_KB} KB limit for a page loaded on demand`);
    failed = true;
  }
}
console.log(`UI size: ${total.toFixed(1)} KB of ${BUDGET_KB} KB for a first visit`);
if (first.size === 0 || total === 0) {
  console.error("found nothing index.html loads: has the build's output changed?");
  failed = true;
}
if (total > BUDGET_KB) {
  console.error(`over the size budget by ${(total - BUDGET_KB).toFixed(1)} KB: split a heavy page out with import()`);
  failed = true;
}
if (failed) process.exit(1);
