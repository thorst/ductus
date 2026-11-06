import { expect, hl7, Kill, type Message, test } from "@ductus/sdk";
import { transform } from "./lab.ts";

test("the lab gets visits, with its own patient class and no Z segments", () => {
  const out = transform(hl7("a01.hl7")) as Message;
  expect(out.get("PV1-2")).toBe("IN");
  expect(out.get("MSH-5")).toBe("LAB");
  expect(out.segmentNames).toEqual(["MSH", "EVN", "PID", "PV1"]);
});

test("the lab doesn't get patient updates (A31)", () => {
  expect(transform(hl7("a31.hl7"))).toBeNull();
});

test("test patients are dropped with a reason", () => {
  const out = transform(hl7("test-patient.hl7"));
  expect(out instanceof Kill && out.reason).toBe("test patient");
});

test("an unknown patient class is quarantined, not guessed", () => {
  const msg = hl7("a01.hl7");
  msg.set("PV1-2", "X");
  expect(() => transform(msg)).toThrow('unknown patient class "X"');
});
