import { expect, input, parseXml, test, type XmlElement } from "@ductus/sdk";
import { transform } from "./orders.ts";

test("the SSN goes, the status and a note come", () => {
  const doc = transform(input<XmlElement>("order.xml"));
  expect(doc.get("Patient/SSN")).toBe("");
  expect(doc.get("Patient/Status")).toBe("verified");
  expect(doc.get("@source")).toBe("ductus");
  expect(doc.get("Note")).toBe("more than one item; the second is CU-17");
  expect(doc.toString()).toContain('<?xml version="1.0" encoding="UTF-8"?>\n<Order id="PO-1001" source="ductus">');
});

test("an order without an MRN is quarantined", () => {
  expect(() => transform(parseXml("<Order><Patient/></Order>"))).toThrow("without Patient/Identifiers/MRN");
});
