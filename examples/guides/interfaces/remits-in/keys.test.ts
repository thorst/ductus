import { expect, fixture, test } from "@ductus/sdk";
import { trace } from "./keys.ts";

test("the dedupe key is the payment's trace number, whatever the separators", () => {
  expect(trace(fixture("remit.835"))).toBe("EFT20261030A");
  const pipes = fixture("remit.835").replaceAll("*", "|").replaceAll("~", "!");
  expect(trace(pipes)).toBe("EFT20261030A");
});

test("a file that isn't X12 has no key: it's quarantined", () => {
  expect(() => trace("MSH|^~\\&|...")).toThrow("not an X12 interchange");
});
