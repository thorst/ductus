// Format tests: run inside the real engine by `cargo test -p ductus-script`.
import { __invoke, expect, kill, parseCsv, parseFixed, parseXml, test, toCsv, toFixed, XmlElement } from "@ductus/sdk";

test("CSV: header records, quotes, embedded delimiters and newlines, CRLF", () => {
  const text = 'mrn,name,note\r\n123,"SMITH, JOHN","said ""hi""\nthen left"\r\n456,DOE,\r\n';
  const rows = parseCsv(text);
  expect(rows).toEqual([
    { mrn: "123", name: "SMITH, JOHN", note: 'said "hi"\nthen left' },
    { mrn: "456", name: "DOE", note: "" },
  ]);
  expect(parseCsv(toCsv(rows))).toEqual(rows);
});

test("CSV: no header, custom delimiter", () => {
  expect(parseCsv("a|b\nc|d", { delimiter: "|", header: false })).toEqual([["a", "b"], ["c", "d"]]);
  expect(toCsv([["x", "y|z"]], { delimiter: "|" })).toBe('x|"y|z"\n');
});

test("CSV: columns in first-seen order, missing values empty", () => {
  expect(toCsv([{ a: "1" }, { b: "2", a: "3" }])).toBe("a,b\n1,\n3,2\n");
});

const layout = {
  fields: [
    { name: "mrn", start: 0, width: 6 },
    { name: "name", start: 6, width: 10 },
    { name: "amount", start: 16, width: 7, align: "right" as const, pad: "0" },
  ],
};

test("fixed-length: read trims padding, write pads and aligns", () => {
  const text = "123456SMITH     0001250\n000789O'BRIEN   0000099\n";
  const records = parseFixed(text, layout);
  expect(records).toEqual([
    { mrn: "123456", name: "SMITH", amount: "0001250" },
    { mrn: "000789", name: "O'BRIEN", amount: "0000099" },
  ]);
  expect(toFixed([{ mrn: "42", name: "AVERYLONGNAMEHERE", amount: "1250" }], layout)).toBe("42    AVERYLONGN0001250\n");
});

const XML = `<?xml version="1.0" encoding="UTF-8"?>
<!-- order feed -->
<Order id="A1">
  <Patient><MRN>123</MRN><Name>O&apos;Brien &amp; Co</Name></Patient>
  <Item code="GLU"><Qty>1</Qty></Item>
  <Item code="NA"><Qty>2</Qty></Item>
  <Note><![CDATA[a < b]]></Note>
  <Empty/>
</Order>`;

test("XML: paths, indexes, attributes, entities, CDATA", () => {
  const doc = parseXml(XML);
  expect(doc.name).toBe("Order");
  expect(doc.get("@id")).toBe("A1");
  expect(doc.get("Patient/MRN")).toBe("123");
  expect(doc.get("Patient/Name")).toBe("O'Brien & Co");
  expect(doc.get("Item[2]/@code")).toBe("NA");
  expect(doc.get("Item[2]/Qty")).toBe("2");
  expect(doc.get("Note")).toBe("a < b");
  expect(doc.get("Missing/Thing")).toBe("");
  expect(doc.elements("Item").map((i) => i.attributes.code)).toEqual(["GLU", "NA"]);
});

test("XML: set creates elements, add/remove, round trip escapes", () => {
  const doc = parseXml(XML);
  doc.set("Patient/MRN", "999");
  doc.set("Visit/Location/@unit", "4W");
  doc.add("Status", "x < y", { by: 'a"b' });
  doc.remove("Empty");
  const again = parseXml(doc.toString());
  expect(again.get("Patient/MRN")).toBe("999");
  expect(again.get("Visit/Location/@unit")).toBe("4W");
  expect(again.get("Status")).toBe("x < y");
  expect(again.get("Status/@by")).toBe('a"b');
  expect(again.find("Empty")).toBe(undefined);
  expect(doc.toString().startsWith('<?xml version="1.0" encoding="UTF-8"?>')).toBe(true);
});

test("XML: rejects malformed documents", () => {
  expect(() => parseXml("<a><b></a>")).toThrow("closes");
  expect(() => parseXml("<a>")).toThrow("unclosed");
});

const ADT = "MSH|^~\\&|EPIC|HOSP|DUCTUS|HOSP|20261005120000||ADT^A08|1|P|2.5\rPID|1||123^^^MRN||SMITH^JOHN\r";
const json = { kind: "json" } as const;
const hl7 = { kind: "hl7" } as const;
const csv = { kind: "csv", delimiter: ",", header: true } as const;

test("formats: HL7 in, JSON out", () => {
  const out = __invoke((m: any) => ({ mrn: m.PID[3][1], last: m.PID[5][1] }), ADT, { input: hl7, output: json });
  expect(out).toBe('{"mrn":"123","last":"SMITH"}');
});

test("formats: JSON in, CSV out", () => {
  const out = __invoke((o: any) => o.patients, '{"patients":[{"mrn":"1","name":"A"},{"mrn":"2","name":"B"}]}', {
    input: json,
    output: csv,
  });
  expect(out).toBe("mrn,name\n1,A\n2,B\n");
});

test("formats: one record per message (split files), and one record out", () => {
  const row = { ...csv, record: true };
  const out = __invoke((r: any) => ({ mrn: r.mrn, name: r.name.toUpperCase() }), "mrn,name\n7,pat", {
    input: row,
    output: csv,
  });
  expect(out).toBe("mrn,name\n7,PAT\n");
  const fixed = { kind: "fixed", record: true, fields: [{ name: "mrn", start: 0, width: 3 }] } as const;
  expect(__invoke((r: any) => r.mrn, "42 ", { input: fixed, output: { kind: "text" } })).toBe("42");
  expect(() => __invoke(() => 5, "{}", { input: json, output: csv })).toThrow("a record or an array");
});

test("formats: XML in and out, strings pass through, kill works for any format", () => {
  const xml = { kind: "xml" } as const;
  const out = __invoke((d: any) => {
    d.set("Patient/MRN", "X");
    return d;
  }, "<Order><Patient><MRN>1</MRN></Patient></Order>", { input: xml, output: xml });
  expect(out).toBe("<Order><Patient><MRN>X</MRN></Patient></Order>");
  expect(__invoke(() => "raw", "{}", { input: json, output: hl7 })).toBe("raw");
  expect(String(__invoke(() => kill("nope"), "{}", { input: json, output: json }))).toContain("nope");
  expect(() => __invoke(() => ({ a: 1 }), ADT, { input: hl7, output: hl7 })).toThrow("output: json");
});

test("XmlElement can be built from scratch", () => {
  const root = new XmlElement("Result");
  root.add("Value", "98", { units: "mg/dL" });
  expect(root.toString()).toBe('<Result><Value units="mg/dL">98</Value></Result>');
});
