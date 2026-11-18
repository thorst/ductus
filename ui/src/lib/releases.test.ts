import { describe, expect, it } from "vitest";
import type { Schemas } from "@/api/client";
import {
  applyOf,
  applySummary,
  changesSummary,
  gateSections,
  reachedOnly,
  reuseCaveat,
  rulesText,
  sectionTitle,
} from "./releases";

const here: Schemas["Here"] = {
  environment: "prod",
  running: false,
  ran_before: false,
  changes: [],
  unchanged: 997,
  project_files_change: false,
  plan: null,
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

describe("applySummary and applyOf", () => {
  const planned = (name: string, what: string[], restarts?: string): Schemas["PlannedChange"] => ({
    interface: name,
    what,
    ...(restarts === undefined ? {} : { restarts }),
  });

  it("says a deploy goes in place, and each interface's line", () => {
    const plan = { changes: [planned("adt", ["code", "destination pacs added"])], handover_because: [], project: [] };
    const h = { ...here, changes: [change("adt", "changed")], plan };
    expect(applySummary(h)).toBe(
      "It applies in place: the interface it changes takes its new code between two messages; the others never pause.",
    );
    expect(applyOf(h, "adt")).toEqual({ what: "code, destination pacs added", how: "in place" });
    expect(applyOf(h, "lab")).toBeNull();
  });

  it("says an interface added or removed in place starts or stops", () => {
    const plan = (changes: Schemas["PlannedChange"][]) => ({
      ...here,
      plan: { changes, handover_because: [], project: [] },
    });
    expect(applySummary(plan([planned("orders", ["added"])]))).toBe(
      "It applies in place: the one it adds starts; the others never pause.",
    );
    expect(
      applySummary(
        plan([planned("adt", ["code"]), planned("a", ["added"]), planned("b", ["added"]), planned("x", ["removed"])]),
      ),
    ).toBe(
      "It applies in place: the interface it changes takes its new code between two messages, the 2 it adds start, the one it removes stops; the others never pause.",
    );
  });

  it("says a source changed in place moves, swaps or restarts", () => {
    const plan = (changes: Schemas["PlannedChange"][]) => ({
      ...here,
      plan: { changes, handover_because: [], project: [] },
    });
    expect(applySummary(plan([planned("lab", ["source"])]))).toBe(
      "It applies in place: the source it changes moves, swaps its settings or restarts alone; the others never pause.",
    );
    expect(
      applySummary(plan([planned("adt", ["code"]), planned("lab", ["source", "code"]), planned("b", ["source"])])),
    ).toBe(
      "It applies in place: the interface it changes takes its new code between two messages, the 2 sources it changes move, swap their settings or restart alone; the others never pause.",
    );
  });

  it("says an interface whose settings a start sets up restarts alone, and why", () => {
    const why = "`ordering` changed (what a start sets up for it: how it stores, lays out in lanes or runs its messages)";
    const plan = { changes: [planned("adt", ["code"]), planned("lab", ["ordering", "source"], why)], handover_because: [], project: [] };
    const h = { ...here, plan };
    expect(applySummary(h)).toBe(
      "It applies in place: the interface it changes takes its new code between two messages, the interface whose settings change restarts alone; the others never pause.",
    );
    expect(applyOf(h, "lab")).toEqual({ what: "ordering, source", how: `in place, restarting alone: ${why}` });
    const two = { ...here, plan: { ...plan, changes: [planned("a", ["format"], why), planned("b", ["workers"], why)] } };
    expect(applySummary(two)).toBe(
      "It applies in place: the 2 interfaces whose settings change restart alone; the others never pause.",
    );
  });

  it("says what needs the handover, and why", () => {
    const why = "resources.yaml changed (read at a start)";
    const plan = { changes: [planned("adt", ["code"])], handover_because: [why], project: [] };
    expect(applySummary({ ...here, plan })).toBe(
      `It needs the handover (a new engine takes over; every interface's sending pauses a moment): ${why}.`,
    );
  });

  it("counts the interfaces a variable's new value reaches, a source it moved, and the environment file alone", () => {
    const port = { name: "MLLP_PORT", before: "6661", after: "6666" };
    const moved = { ...planned("feed", ["source (MLLP_PORT)"]), variables: [port] };
    const both = { ...planned("adt", ["code", "code reads EMPI_URL"]), variables: [port] };
    const h = {
      ...here,
      changes: [change("adt", "changed")],
      unchanged: 4,
      plan: {
        changes: [both, moved],
        handover_because: [],
        project: ['environments/prod.yaml: MLLP_PORT "6661" → "6666"'],
      },
    };
    expect(reachedOnly(h).map((c) => c.interface)).toEqual(["feed"]);
    expect(changesSummary(h)).toBe("1 interface changes, 1 interface takes new variable values; 3 as they run now.");
    expect(applySummary(h)).toBe(
      "It applies in place: the interface it changes takes its new code between two messages, the source it changes moves, swaps its settings or restarts alone; the others never pause.",
    );
    const alone = {
      ...here,
      plan: { changes: [], handover_because: [], project: ["environments/prod.yaml: console info → warn"] },
    };
    expect(applySummary(alone)).toBe("It applies in place: no interface changes.");
  });

  it("says nothing when the engine didn't, it runs it, or nothing changes", () => {
    expect(applySummary(here)).toBeNull();
    expect(applySummary({ ...here, plan: { changes: [], handover_because: [], project: [] } })).toBeNull();
    const plan = { changes: [planned("adt", ["code"])], handover_because: [], project: [] };
    expect(applySummary({ ...here, running: true, plan })).toBeNull();
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
