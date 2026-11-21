// SDK tests: run inside the real engine by `cargo test -p ductus-script`.
// The shared table (key-paths.json) is run by the engine's key reader too
// (crates/ductus-runtime/src/keys.rs): a path reads the same in interface.yaml and in code.
import { expect, fixture, getPath, parse, parseXml, test, XmlElement } from "@ductus/sdk";

interface Case {
  format: "hl7" | "json" | "xml";
  names?: "local" | "qualified";
  message?: string;
  path: string;
  value?: string;
  refused?: boolean;
}

interface Table {
  messages: Record<string, unknown>;
  cases: Case[];
}

function decoded(table: Table, c: Case): unknown {
  const message = table.messages[c.message!];
  const text = typeof message === "string" ? message : JSON.stringify(message);
  switch (c.format) {
    case "hl7":
      return parse(text);
    case "xml":
      return parseXml(text, { names: c.names });
    case "json":
      return JSON.parse(text);
  }
}

test("every case of the shared table reads as the engine reads it", () => {
  const table = JSON.parse(fixture("key-paths.json")) as Table;
  expect(table.cases.length >= 40).toBe(true);
  const wrong: string[] = [];
  for (const c of table.cases) {
    if (c.refused) {
      // A message of the format, so a refusal is the path's, not the message's.
      const sample = c.format === "hl7" ? parse("MSH|^~\\&|A\r") : c.format === "xml" ? parseXml("<a/>") : {};
      let threw = false;
      try {
        getPath(sample, c.path);
      } catch {
        threw = true;
      }
      if (!threw) wrong.push(`${c.format} ${c.path}: should be refused`);
      continue;
    }
    const got = getPath(decoded(table, c), c.path);
    if (got !== c.value) wrong.push(`${c.format}${c.names ? ` (${c.names})` : ""} ${c.path}: expected "${c.value}", got "${got}"`);
  }
  expect(wrong).toEqual([]);
});

test("a refused path says why", () => {
  expect(() => parse("MSH|^~\\&|A\r").get("PID-3[.5]")).toThrow("a filter is `.component=value`");
  expect(() => getPath({}, "a[x]")).toThrow("a filter is `name=value`");
  expect(() => parseXml("<a/>").get("id[0]")).toThrow("XML positions count from 1");
});

test("msg.set takes a field or a component, and says how to reach a repetition", () => {
  const msg = parse("MSH|^~\\&|A\rPID|1||1^^^A^MR~2^^^B^PI\r");
  expect(() => msg.set("PID-3[.5=MR].1", "X")).toThrow("msg.PID[3].rep(2)[1] = value");
  msg.PID[3].rep(2)[1] = "22";
  expect(msg.get("PID-3[.5=PI].1")).toBe("22");
});

test("XML names match by local name unless qualified", () => {
  const xml = "<hl7:Doc xmlns:hl7='urn:hl7-org:v3'><hl7:Patient><hl7:MRN>1</hl7:MRN></hl7:Patient></hl7:Doc>";
  const local = parseXml(xml);
  expect(local.get("Patient/MRN")).toBe("1");
  expect(local.find("Patient")?.name).toBe("hl7:Patient");
  expect(local.elements("Patient").length).toBe(1);
  local.set("Patient/Status", "ok");
  expect(local.get("Patient/Status")).toBe("ok");
  local.find("Patient")?.remove("MRN");
  expect(local.get("Patient/MRN")).toBe("");

  const qualified = parseXml(xml, { names: "qualified" });
  expect(qualified.get("Patient/MRN")).toBe("");
  expect(qualified.get("hl7:Patient/hl7:MRN")).toBe("1");
  expect(new XmlElement("a").names).toBe("local");
});

test("XML filters find an element; set takes names and positions only", () => {
  const doc = parseXml("<o><Item code='A'><Qty>1</Qty></Item><Item code='B'><Qty>2</Qty></Item></o>");
  expect(doc.find("Item[@code=B]")?.get("Qty")).toBe("2");
  expect(doc.get("Item[Qty=1]/@code")).toBe("A");
  expect(() => doc.set("Item[@code=B]/Qty", "3")).toThrow("find the element first");
  doc.find("Item[@code=B]")?.set("Qty", "3");
  expect(doc.get("Item[2]/Qty")).toBe("3");
});

test("getPath reads any decoded message", () => {
  expect(getPath(parse("MSH|^~\\&|A\rPID|1||9\r"), "PID-3.1")).toBe("9");
  expect(getPath(parseXml("<a><b>2</b></a>"), "b")).toBe("2");
  expect(getPath({ a: [{ b: 1 }, { b: 2, c: { d: "x" } }] }, "a[b=2].c")).toBe('{"d":"x"}');
  expect(getPath([{ id: "x" }], "[0].id")).toBe("x");
  expect(getPath(null, "a")).toBe("");
});
