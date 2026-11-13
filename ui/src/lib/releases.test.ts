import { describe, expect, it } from "vitest";
import type { Schemas } from "@/api/client";
import { changesSummary, gateSections, reuseCaveat, rulesText, sectionTitle } from "./releases";

const here: Schemas["Here"] = {
  environment: "prod",
  running: false,
  ran_before: false,
  changes: [],
  unchanged: 997,
  project_files_change: false,
};

const change = (interfaceName: string, kind: string): Schemas["HereChange"] => ({
  interface: interfaceName,
  change: kind,
  from: kind === "new" ? null : "aaaaaaaaaaaa",
  to: kind === "removed" ? null : "bbbbbbbbbbbb",
});

const mode: Schemas["ReleaseMode"] = {
  project: "integrations",
  running: null,
  requires: [],
  branches: [],
  reuse_check_seconds: 3600,
  promote_from: null,
};

describe("changesSummary", () => {
  it("counts what changes, what's new and what goes, then the rest", () => {
    const changes = [
      change("adt", "changed"),
      change("lab", "changed"),
      change("orders", "new"),
      change("x", "removed"),
    ];
    expect(changesSummary({ ...here, changes })).toBe(
      "2 interfaces change, 1 interface new here, 1 interface removed; 997 as they run now.",
    );
    expect(changesSummary({ ...here, changes: [change("adt", "changed")], unchanged: 1 })).toBe(
      "1 interface changes; 1 as they run now.",
    );
  });

  it("says when nothing changes, and when the project's own files do", () => {
    expect(changesSummary({ ...here, unchanged: 3 })).toBe("No interface changes: all 3 run here as they are.");
    expect(changesSummary({ ...here, unchanged: 3, project_files_change: true })).toContain(
      "The project's own files (ductus.yaml, resources.yaml, environments, secrets) change too.",
    );
    expect(changesSummary({ ...here, running: true })).toBe("It runs here now: deploying it changes nothing.");
  });
});

describe("rulesText", () => {
  it("says each rule, and how checks are reused", () => {
    expect(rulesText({ ...mode, requires: ["test", "uat"], branches: ["main", "release/*"] }, "prod")).toEqual([
      "prod takes only releases deployed to test and uat first.",
      "prod takes only releases made on main or release/*.",
      "A passing check of the same release is reused for 1 h when nothing was deployed here since.",
    ]);
    expect(rulesText({ ...mode, reuse_check_seconds: 0 }, "prod")).toEqual([
      "The gate runs at every deploy (no check is reused).",
    ]);
  });

  it("warns that a check first won't be reused when reuse is off", () => {
    expect(reuseCaveat(mode)).toBeNull();
    expect(reuseCaveat({ ...mode, reuse_check_seconds: 0 })).toContain("runs the gate again");
  });
});

describe("gateSections", () => {
  it("groups the report's lines by section, in order, and reads unknown levels as notes", () => {
    const report = {
      checks: [
        { section: "release", level: "ok", text: "release 7fc9677466dd", more: [] },
        { section: "environment", level: "ok", text: "1 interface(s) change", more: [] },
        { section: "environment", level: "note", text: "changed:", more: ["adt: a → b"] },
        { section: "tests", level: "problem", text: "1 failed", more: ["expected LAB"] },
        { section: "tests", level: "odd", text: "?", more: [1] },
      ],
    };
    const sections = gateSections(report);
    expect(sections.map((s) => [s.section, s.checks.length])).toEqual([
      ["release", 1],
      ["environment", 2],
      ["tests", 2],
    ]);
    expect(sections[1]?.checks[1]?.more).toEqual(["adt: a → b"]);
    expect(sections[2]?.checks[1]).toEqual({ section: "tests", level: "note", text: "?", more: [] });
    expect(gateSections(null)).toEqual([]);
    expect(gateSections({ checks: "no" })).toEqual([]);
  });

  it("titles sections as the CLI prints them", () => {
    expect(sectionTitle("environment", "prod")).toBe("What it changes in prod");
    expect(sectionTitle("tests", "prod")).toBe("Tests");
  });
});
