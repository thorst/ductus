import { describe, expect, it } from "vitest";
import {
  dateText,
  isWebUrl,
  entryNamed,
  fieldsFor,
  matches,
  missingCounts,
  NO_VALUE,
  texts,
  valuesFor,
  type DocsEntry,
  type DocsField,
  type DocsReport,
} from "./docs";

const field = (key: string, over: Partial<DocsField> = {}): DocsField => ({
  key,
  title: key,
  type: "text",
  values: [],
  required: false,
  applies_to: ["interface"],
  ...over,
});
const entry = (
  name: string,
  custom: Record<string, unknown>,
  missing: string[] = [],
  level = "interface",
): DocsEntry => ({
  level,
  name,
  interface: level === "resource" ? null : name.split(".")[0],
  custom,
  missing,
});

const baa = field("baa_signed", {
  type: "enum",
  values: ["yes", "no", "n/a"],
  required: true,
  applies_to: ["interface", "resource"],
});
const downstream = field("downstream", { type: "list" });
const report: DocsReport = {
  fields: [baa, downstream, field("runbook", { applies_to: ["interface", "destination"] })],
  entries: [
    entry("adt", { baa_signed: "yes", downstream: ["lab", "billing"] }),
    entry("orders", { baa_signed: "no" }),
    entry("results", { baa_signed: "  " }, ["baa_signed"]),
    entry("adt.lab", { runbook: "https://w/x" }, [], "destination"),
    entry("api empi", {}, ["baa_signed"], "resource"),
  ],
  problems: [],
};

describe("documentation values", () => {
  it("reads texts from text, lists and nothing", () => {
    expect(texts("PHI")).toEqual(["PHI"]);
    expect(texts("  ")).toEqual([]);
    expect(texts(["a", " ", "b"])).toEqual(["a", "b"]);
    expect(texts(undefined)).toEqual([]);
    expect(texts(5)).toEqual([]);
  });

  it("filters by a value, a list item, or no value, only where the field applies", () => {
    expect(report.entries.filter((e) => matches(e, baa, "no")).map((e) => e.name)).toEqual(["orders"]);
    expect(report.entries.filter((e) => matches(e, downstream, "lab")).map((e) => e.name)).toEqual(["adt"]);
    // "No BAA value": the blank one and the resource, not the destination (the field doesn't apply to it).
    expect(report.entries.filter((e) => matches(e, baa, NO_VALUE)).map((e) => e.name)).toEqual(["results", "api empi"]);
  });

  it("offers an enum's declared values, else the values in use", () => {
    expect(valuesFor(baa, report.entries)).toEqual(["yes", "no", "n/a"]);
    expect(valuesFor(downstream, report.entries)).toEqual(["billing", "lab"]);
  });

  it("counts what is missing per required field, among the entries it applies to", () => {
    expect(missingCounts(report)).toEqual([{ field: baa, missing: 2, of: 4 }]);
  });

  it("finds fields and entries by level and name", () => {
    expect(fieldsFor(report, "destination").map((f) => f.key)).toEqual(["runbook"]);
    expect(entryNamed(report, "destination", "adt.lab")?.custom.runbook).toBe("https://w/x");
    expect(entryNamed(report, "interface", "nope")).toBeUndefined();
  });

  it("links only http(s) addresses", () => {
    expect(isWebUrl("https://wiki.example.org/x")).toBe(true);
    expect(isWebUrl("http://10.1.2.3:8080")).toBe(true);
    expect(isWebUrl("javascript:alert(1)")).toBe(false);
    expect(isWebUrl("wiki")).toBe(false);
    expect(isWebUrl("https://")).toBe(false);
  });

  it("writes a date out, and anything else as written", () => {
    expect(dateText("2026-10-13")).toBe("Oct 13, 2026");
    expect(dateText("soon")).toBe("soon");
  });
});
