import { describe, expect, it } from "vitest";
import type { Schemas } from "@/api/client";
import { deployedBy } from "./deployment";

const base: Schemas["DeploymentInfo"] = {
  id: 7,
  at: "2026-11-04T14:02:00Z",
  how: "handover",
  by: "todd",
  reason: null,
  git_commit: null,
  uncommitted: null,
  changed: 3,
};

describe("deployedBy", () => {
  it("says who and how many interfaces changed", () => {
    expect(deployedBy(base)).toBe("by todd (3 interfaces changed)");
    expect(deployedBy({ ...base, changed: 0 })).toBe("by todd (nothing changed)");
    expect(deployedBy({ ...base, changed: 1 })).toBe("by todd (1 interface changed)");
  });

  it("adds why, the commit and what isn't committed", () => {
    const d = { ...base, reason: "fix PV1-3", git_commit: "3f9c2e81b07a55aa00", uncommitted: ["lib/codes.ts", "x.ts"] };
    expect(deployedBy(d)).toBe(
      "by todd (3 interfaces changed): fix PV1-3 · commit 3f9c2e81b07a, 2 files not committed",
    );
    expect(deployedBy({ ...d, uncommitted: [] })).toBe(
      "by todd (3 interfaces changed): fix PV1-3 · commit 3f9c2e81b07a",
    );
  });
});
