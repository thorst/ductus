// Starts a real VS Code (downloaded once into .vscode-test, @vscode/test-electron) with the
// extension from this folder, on a copy of the guides' example project, and runs tests.ts in it.
//   DUCTUS_BIN=~/target/release/ductus node dist/test/e2e/run.js
// On a machine without a display, under xvfb-run (scripts/demo-vscode.sh does).
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { runTests } from "@vscode/test-electron";

async function main() {
  const extension = path.resolve(__dirname, "../../..");
  const bin = process.env.DUCTUS_BIN;
  if (!bin || !fs.existsSync(bin)) throw new Error(`set DUCTUS_BIN to the ductus binary (got ${bin ?? "nothing"})`);

  const work = fs.mkdtempSync(path.join(os.tmpdir(), "ductus-vscode-"));
  const project = path.join(work, "guides");
  fs.cpSync(path.join(extension, "../../examples/guides"), project, {
    recursive: true,
    filter: (src) => !src.includes(`${path.sep}.ductus`),
  });
  // No launch.json: the extension's own debug configurations are what's tested.
  fs.mkdirSync(path.join(project, ".vscode"), { recursive: true });
  fs.writeFileSync(
    path.join(project, ".vscode/settings.json"),
    JSON.stringify({ "ductus.path": path.resolve(bin), "ductus.validateOnSave": true }, null, 2),
  );
  try {
    await runTests({
      extensionDevelopmentPath: extension,
      extensionTestsPath: path.join(__dirname, "suite.js"),
      // Short folders: the user data folder holds a socket, whose path has a length limit.
      launchArgs: [
        project,
        "--disable-extensions",
        "--disable-workspace-trust",
        "--skip-welcome",
        "--skip-release-notes",
        `--user-data-dir=${path.join(work, "u")}`,
      ],
      // DUCTUS_E2E_ENGINES: the engines Promote… is tested against (scripts/demo-vscode.sh).
      extensionTestsEnv: { DUCTUS_E2E_PROJECT: project, DUCTUS_E2E_ENGINES: process.env.DUCTUS_E2E_ENGINES ?? "" },
    });
  } finally {
    if (!process.env.DUCTUS_E2E_KEEP) fs.rmSync(work, { recursive: true, force: true });
  }
}

main().catch((e) => {
  console.error(e instanceof Error ? e.message : e);
  process.exit(1);
});
