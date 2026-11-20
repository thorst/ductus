// Bundles the extension into one file (dist/extension.js: fast to load, small to ship) and the
// end-to-end tests beside it (dist/test/e2e, never packaged). `--production` minifies the
// extension and leaves out source maps.
import * as esbuild from "esbuild";

const production = process.argv.includes("--production");
const common = {
  bundle: true,
  platform: "node",
  format: "cjs",
  target: "node20",
  external: ["vscode", "mocha"],
  logLevel: "warning",
};

await esbuild.build({
  ...common,
  entryPoints: ["src/extension.ts"],
  outfile: "dist/extension.js",
  minify: production,
  sourcemap: !production,
});

if (!production) {
  await esbuild.build({
    ...common,
    entryPoints: ["test/e2e/run.ts", "test/e2e/suite.ts", "test/e2e/tests.ts"],
    outdir: "dist/test/e2e",
    sourcemap: true,
  });
}
