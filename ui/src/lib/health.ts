// The engine's health states (decisions.md "Monitoring"), ranked so a list can show the worst
// first. Interfaces: ok / silent / missed / stopped. Destinations: ok / behind / error /
// stalled / paused. A paused destination was paused on purpose: shown, but not an alarm.

import type { Schemas } from "@/api/client";

export type Tone = "ok" | "warn" | "bad" | "paused";

const STATES: Record<string, { tone: Tone; rank: number; label: string }> = {
  ok: { tone: "ok", rank: 0, label: "OK" },
  paused: { tone: "paused", rank: 1, label: "Paused" },
  behind: { tone: "warn", rank: 2, label: "Behind" },
  silent: { tone: "warn", rank: 2, label: "Silent" },
  error: { tone: "bad", rank: 3, label: "Errors" },
  missed: { tone: "bad", rank: 4, label: "Missed" },
  stalled: { tone: "bad", rank: 5, label: "Stalled" },
  stopped: { tone: "bad", rank: 6, label: "Stopped" },
};

export function describe(state: string) {
  return STATES[state] ?? { tone: "warn" as Tone, rank: 2, label: state };
}

export interface Problem {
  /** The destination, or none for the interface itself. */
  destination?: string;
  state: string;
  summary: string;
}

/** What isn't ok in an interface, worst first. */
export function problems(i: Schemas["InterfaceStatus"]): Problem[] {
  const found: Problem[] = [];
  if (i.state === "stopped") found.push({ state: "stopped", summary: i.stopped_reason ?? "stopped" });
  else if (i.health.state !== "ok") found.push({ state: i.health.state, summary: i.health.summary });
  for (const d of i.destinations) {
    if (d.health.state !== "ok") found.push({ destination: d.name, state: d.health.state, summary: d.health.summary });
  }
  return found.sort((a, b) => describe(b.state).rank - describe(a.state).rank);
}

/** The interface's worst state, its destinations' included. */
export function worst(i: Schemas["InterfaceStatus"]): string {
  return problems(i)[0]?.state ?? "ok";
}
