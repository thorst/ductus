import { expect, getPath, input, test } from "@ductus/sdk";
import { transform } from "./patients.ts";

test("the MRN is the identifier of our system, not the first one", () => {
  const patient = input<Record<string, unknown>>("patient_1001.json");
  expect(transform(patient)).toEqual({ mrn: "100234", family: "Rivera" });
});

test("the ordering key reads the same MRN", () => {
  // interface.yaml's `ordering.key`: the same path, so the engine orders by this value.
  expect(getPath(input("patient_1001.json"), "identifier[system=urn:oid:1.2.3].value")).toBe("100234");
});

test("a Patient without our MRN is quarantined", () => {
  const other = { resourceType: "Patient", identifier: [{ system: "urn:oid:9.9", value: "1" }] };
  expect(() => transform(other)).toThrow("without our MRN");
});
