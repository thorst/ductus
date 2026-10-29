import { describe, expect, it } from "vitest";
import type { Schemas } from "@/api/client";
import { summary } from "./audit-table";

const entry = (action: string, detail: Record<string, unknown>): Schemas["AuditEntry"] => ({
  id: 1,
  at: "2026-10-29T14:02:00Z",
  action,
  actor: "engine",
  actor_kind: "system",
  outcome: "ok",
  via: "engine",
  detail: detail as Schemas["AuditEntry"]["detail"],
});

describe("summary", () => {
  it("says which version handed over to which, and how long sending paused", () => {
    const detail = {
      from: { version: "0.6.0", pid: 4120, instance: "a" },
      to: { version: "0.6.1", pid: 5233, instance: "b" },
      sending_paused_ms: 180,
    };
    expect(summary(entry("engine.handover", detail))).toBe(
      "v0.6.0 (pid 4120) → v0.6.1 (pid 5233) · sending paused 180 ms",
    );
  });

  it("names the lanes a refused handover would have lowered", () => {
    const detail = { reason: "handover_lanes", lowered: [{ interface: "adt", before: 64, now: 32 }] };
    expect(summary(entry("engine.refused", detail))).toBe(
      "a handover can't lower lanes (adt: 64 → 32): restart instead",
    );
  });
});
