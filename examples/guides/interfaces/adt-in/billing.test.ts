import { expect, hl7, test } from "@ductus/sdk";
import { transform } from "./billing.ts";

test("billing gets admissions and discharges only", () => {
  expect(transform(hl7("a01.hl7"))).toBeTruthy();
  expect(transform(hl7("a03.hl7"))).toBeTruthy();
  expect(transform(hl7("a31.hl7"))).toBeNull();
});
