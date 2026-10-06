import { expect, hl7, parse, test } from "@ductus/sdk";
import { transform } from "./transform.ts";

test("relays ADT with a ZDU tag", () => {
  const out = parse(String(transform(hl7("adt-a08.hl7"))));
  expect(out.PID[3][1]).toBe("123456");
  expect(out.ZDU[1].value).toBe("relayed-by-ductus");
  expect(out.segmentNames).toEqual(["MSH", "EVN", "PID", "PV1", "ZDU"]);
});

test("filters out non-ADT messages", () => {
  expect(transform(hl7("orm-o01.hl7"))).toBeNull();
});
