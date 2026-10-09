// The UI's size budget: what a first visit downloads (JavaScript and CSS, gzipped as the engine
// serves them). Over it, the build fails: split a heavy page out with a dynamic import() first.
import { readdirSync, readFileSync } from "node:fs";
import { gzipSync } from "node:zlib";

const BUDGET_KB = 300;
const dir = new URL("../dist/assets/", import.meta.url);
const files = readdirSync(dir).filter((f) => /\.(js|css)$/.test(f));
const sizes = files.map((f) => [f, gzipSync(readFileSync(new URL(f, dir)), { level: 9 }).length / 1024]);
const total = sizes.reduce((sum, [, kb]) => sum + kb, 0);
for (const [f, kb] of sizes) console.log(`  ${f}  ${kb.toFixed(1)} KB gzipped`);
console.log(`UI size: ${total.toFixed(1)} KB of ${BUDGET_KB} KB`);
if (total > BUDGET_KB) {
  console.error(`over the size budget by ${(total - BUDGET_KB).toFixed(1)} KB`);
  process.exit(1);
}
