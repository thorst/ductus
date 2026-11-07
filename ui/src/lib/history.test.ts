import { describe, expect, it } from "vitest";
import type { Schemas } from "@/api/client";
import { decodePart, encodePart, surely, touched } from "./history";

type Entry = Schemas["HistoryEntry"];

function entry(kind: Entry["kind"], touches: Partial<Entry["touches"]>): Entry {
  return {
    at: "2026-11-06T10:00:00Z",
    kind,
    by: "ann",
    what: "something",
    key: "1",
    touches: { everything: false, source: false, destinations: [], unsure: false, ...touches },
  };
}

describe("history parts", () => {
  it("says which parts an entry touches", () => {
    expect(touched(entry("commit", { source: true, destinations: ["billing", "lab"] }))).toBe("source, billing, lab");
    expect(touched(entry("commit", { everything: true }))).toBe("every part");
    expect(touched(entry("deployment", { everything: true }))).toBe("");
    expect(touched(entry("documentation", {}))).toBe("");
    expect(touched(entry("deployment", { unsure: true }))).toBe("code of parts not recorded (before 0.7.4.2)");
  });

  it("tells an entry known to touch a part from one that only may", () => {
    const lab = { kind: "destination", name: "lab" } as const;
    expect(surely(entry("audit", { destinations: ["lab"] }), lab)).toBe(true);
    expect(surely(entry("deployment", { unsure: true }), lab)).toBe(false);
    expect(surely(entry("deployment", { everything: true }), lab)).toBe(true);
    expect(surely(entry("commit", { source: true }), { kind: "source" })).toBe(true);
    expect(surely(entry("commit", { unsure: true }), { kind: "whole" })).toBe(true);
  });

  it("keeps the picked part in the picker's value", () => {
    for (const part of [
      { kind: "whole" },
      { kind: "source" },
      { kind: "destination", name: "lab" },
      { kind: "destination", name: "source" },
    ] as const) {
      expect(decodePart(encodePart(part))).toEqual(part);
    }
  });
});
