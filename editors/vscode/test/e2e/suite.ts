// What VS Code runs inside its extension host (`extensionTestsPath`): Mocha on tests.js.
import * as path from "node:path";
import Mocha from "mocha";

export function run(): Promise<void> {
  const mocha = new Mocha({ ui: "tdd", timeout: 90_000, color: false, reporter: "spec" });
  mocha.addFile(path.join(__dirname, "tests.js"));
  return new Promise((resolve, reject) => {
    mocha.run((failures) => (failures > 0 ? reject(new Error(`${failures} test(s) failed`)) : resolve()));
  });
}
