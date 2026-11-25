import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import { EXTENSION, debugInVsCode } from "./vscode";

describe("Debug in VS Code", () => {
  it("links to the extension with the engine, the interface and the message", () => {
    expect(debugInVsCode("https://ductus-prod:7700/", "adt-in", 42)).toBe(
      "vscode://getductus.ductus/debug-message?engine=https%3A%2F%2Fductus-prod%3A7700&interface=adt-in&message=42",
    );
  });

  it("names the extension as its package.json does", () => {
    const manifest = JSON.parse(readFileSync(resolve(__dirname, "../../../editors/vscode/package.json"), "utf8"));
    expect(EXTENSION).toBe(`${manifest.publisher}.${manifest.name}`);
  });
});
