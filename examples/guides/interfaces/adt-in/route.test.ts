import { expect, hl7, Kill, type Message, test } from "@ductus/sdk";
import { toBilling, toLab } from "./route.ts";

test("the lab gets visits, with its own patient class and no Z segments", () => {
  const out = toLab(hl7("a01.hl7")) as Message;
  expect(out.get("PV1-2")).toBe("IN");
  expect(out.get("MSH-5")).toBe("LAB");
  expect(out.segmentNames).toEqual(["MSH", "EVN", "PID", "PV1"]);
});

test("the lab doesn't get patient updates (A31)", () => {
  expect(toLab(hl7("a31.hl7"))).toBeNull();
});

test("test patients are dropped with a reason", () => {
  const out = toLab(hl7("test-patient.hl7"));
  expect(out instanceof Kill && out.reason).toBe("test patient");
});

test("an unknown patient class is quarantined, not guessed", () => {
  const msg = hl7("a01.hl7");
  msg.set("PV1-2", "X");
  expect(() => toLab(msg)).toThrow('unknown patient class "X"');
});

test("billing gets admissions and discharges only", () => {
  expect(toBilling(hl7("a01.hl7"))).toBeTruthy();
  expect(toBilling(hl7("a03.hl7"))).toBeTruthy();
  expect(toBilling(hl7("a31.hl7"))).toBeNull();
});
