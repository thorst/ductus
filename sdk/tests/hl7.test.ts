// SDK tests: run inside the real engine by `cargo test -p ductus-script`.
import { expect, hl7, parse, test } from "@ductus/sdk";

const ADT =
  "MSH|^~\\&|EPIC|HOSP|DUCTUS|HOSP|20261005120000||ADT^A08^ADT_A01|MSG0001|P|2.5\r" +
  "EVN|A08|20261005120000\r" +
  "PID|1||123456^^^MRN^MR~999-88-7777^^^SSA^SS||SMITH^JOHN^Q||19800101|M|||1 MAIN ST^^YORK^PA^17404\r" +
  "PV1|1|I|4W^401^A\r" +
  "OBX|1|NM|GLU^Glucose||98|mg/dL\r" +
  "OBX|2|NM|NA^Sodium||140|mmol/L\r";

test("reads fields with HL7 numbering", () => {
  const msg = parse(ADT);
  expect(msg.MSH[3][1]).toBe("EPIC");
  expect(msg.MSH[9][2]).toBe("A08");
  expect(msg.MSH[10]).toBe("MSG0001");
  expect(msg.PID[3][1]).toBe("123456");
  expect(msg.PID[5][1]).toBe("SMITH");
  expect(msg.PID[5][2]).toBe("JOHN");
  expect(msg.PID[8]).toBe("M");
  expect(msg.PID[11][3]).toBe("YORK");
});

test("MSH-1 and MSH-2 are the delimiters", () => {
  const msg = parse(ADT);
  expect(msg.MSH[1]).toBe("|");
  expect(msg.MSH[2]).toBe("^~\\&");
});

test("primitive fields are plain strings, composite fields are Fields", () => {
  const msg = parse(ADT);
  expect(typeof msg.PID[8]).toBe("string");
  expect(typeof msg.PID[3]).toBe("object");
  expect(msg.PID[3].value).toBe("123456^^^MRN^MR");
  expect(String(msg.PID[3])).toBe("123456^^^MRN^MR~999-88-7777^^^SSA^SS");
});

test("missing segments and fields read as empty", () => {
  const msg = parse(ADT);
  expect(msg.NK1[2][1]).toBe("");
  expect(msg.PID[30]).toBe("");
  expect(msg.PID[5][9]).toBe("");
  expect(msg.ZPI[1].value).toBe("");
  expect(msg.NK1.exists).toBe(false);
});

test("iterating goes across segment repetitions", () => {
  const msg = parse(ADT);
  expect(msg.OBX.count).toBe(2);
  expect(msg.OBX[3][1]).toBe("GLU");
  const codes: string[] = [];
  for (const obx of msg.OBX) codes.push(obx[3][1]);
  expect(codes).toEqual(["GLU", "NA"]);
  expect(msg.OBX.rep(2)[5].value).toBe("140");
});

test("iterating goes across field repetitions", () => {
  const msg = parse(ADT);
  expect(msg.PID[3].count).toBe(2);
  const ids: string[] = [];
  for (const id of msg.PID[3]) ids.push(`${id[1]}/${id[5]}`);
  expect(ids).toEqual(["123456/MR", "999-88-7777/SS"]);
  expect(msg.PID[3].rep(2)[1]).toBe("999-88-7777");
});

test("writes components and fields in place", () => {
  const msg = parse(ADT);
  msg.PID[5][1] = "JONES";
  msg.PID[8] = "F";
  msg.PV1[3][2] = "402";
  expect(msg.PID[5][1]).toBe("JONES");
  expect(msg.PID[5][2]).toBe("JOHN");
  expect(msg.PID[8]).toBe("F");
  expect(msg.toString()).toContain("PID|1||123456^^^MRN^MR~999-88-7777^^^SSA^SS||JONES^JOHN^Q||19800101|F|");
  expect(msg.toString()).toContain("PV1|1|I|4W^402^A\r");
});

test("writes extend short fields and segments", () => {
  const msg = parse("MSH|^~\\&|A\rPID|1\r");
  msg.PID[5][2] = "ANN";
  expect(msg.toString()).toBe("MSH|^~\\&|A\rPID|1||||^ANN\r");
});

test("untouched segments are written back exactly", () => {
  const raw = "MSH|^~\\&|A|B|||20260101||ADT^A01|1|P|2.5\rPID|1||X^^^Y||ODD\\T\\NAME\rZZZ|a|b\r";
  expect(parse(raw).toString()).toBe(raw);
});

test("escape sequences are decoded on read and encoded on write", () => {
  const msg = parse("MSH|^~\\&|A\rPID|1||||O\\S\\BRIEN^PAT\r");
  expect(msg.PID[5][1]).toBe("O^BRIEN");
  msg.PID[5][2] = "A|B";
  expect(msg.toString()).toContain("O\\S\\BRIEN^A\\F\\B");
  expect(msg.PID[5][2]).toBe("A|B");
});

test("path helpers", () => {
  const msg = parse(ADT);
  expect(msg.get("PID-3.1")).toBe("123456");
  expect(msg.get("PID-3")).toBe("123456^^^MRN^MR");
  expect(msg.get("NK1-2.1")).toBe("");
  msg.set("PID-5.1", "DOE");
  expect(msg.PID[5][1]).toBe("DOE");
  expect(() => msg.get("bad")).toThrow("invalid HL7 path");
});

test("add, insertAfter and remove segments", () => {
  const msg = parse(ADT);
  msg.insertAfter("PID", "ZPI", ["1", "VIP"]);
  msg.add("NTE", ["1", "L", "note"]);
  msg.OBX.remove();
  expect(msg.segmentNames).toEqual(["MSH", "EVN", "PID", "ZPI", "PV1", "NTE"]);
  expect(msg.ZPI[2].value).toBe("VIP");
});

test("writing to a missing segment explains how to fix it", () => {
  const msg = parse(ADT);
  expect(() => {
    msg.NK1[2] = "X";
  }).toThrow('msg.add("NK1")');
});

test("MSH delimiters are read-only", () => {
  const msg = parse(ADT);
  expect(() => {
    msg.MSH[1] = "#";
  }).toThrow("read-only");
});

test("custom delimiters", () => {
  const msg = parse("MSH#!*%@#APP\rPID#1##123!!!MRN\r");
  expect(msg.MSH[3].value).toBe("APP");
  expect(msg.PID[3][1]).toBe("123");
  expect(msg.PID[3][4]).toBe("MRN");
});

test("rejects non-HL7", () => {
  expect(() => parse("PID|1")).toThrow("must start with an MSH");
});

test("fixtures load and parse", () => {
  expect(hl7("adt.hl7").PID[3][1]).toBe("123456");
});
