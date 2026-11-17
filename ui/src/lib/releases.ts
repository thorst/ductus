// What the Releases pages say: a check's report as sections of lines, what a release changes
// here, and this environment's rules, worded once (and tested) instead of in each component.

import type { Schemas } from "@/api/client";
import { count, duration } from "@/lib/format";

export type Level = "ok" | "note" | "warning" | "problem";

/** One line of the gate's report (`ductus release check --json`'s `checks`). */
export interface GateCheck {
  section: string;
  level: Level;
  text: string;
  more: string[];
}

/** The report's sections in the order the gate runs them, with the titles the CLI prints. */
export function sectionTitle(section: string, environment: string): string {
  switch (section) {
    case "release":
      return "The release";
    case "environment":
      return `What it changes in ${environment}`;
    case "validate":
      return "validate";
    case "tests":
      return "Tests";
    default:
      return section;
  }
}

const LEVELS: readonly Level[] = ["ok", "note", "warning", "problem"];

/** A check's report read defensively (it's JSON the CLI wrote): its lines grouped by section, in order. */
export function gateSections(report: unknown): { section: string; checks: GateCheck[] }[] {
  const raw = (report as { checks?: unknown } | null)?.checks;
  if (!Array.isArray(raw)) return [];
  const sections: { section: string; checks: GateCheck[] }[] = [];
  for (const item of raw as Record<string, unknown>[]) {
    const level = LEVELS.find((l) => l === item.level) ?? "note";
    const check: GateCheck = {
      section: typeof item.section === "string" ? item.section : "",
      level,
      text: typeof item.text === "string" ? item.text : "",
      more: Array.isArray(item.more) ? item.more.filter((m): m is string => typeof m === "string") : [],
    };
    const last = sections.at(-1);
    if (last && last.section === check.section) last.checks.push(check);
    else sections.push({ section: check.section, checks: [check] });
  }
  return sections;
}

const interfaces = (n: number) => (n === 1 ? "1 interface" : `${count(n)} interfaces`);

/** "2 interfaces change, 1 new here, 1 removed; 997 as they run now", or what's true when nothing does. */
export function changesSummary(here: Schemas["Here"]): string {
  if (here.running) return "It runs here now: deploying it changes nothing.";
  const of = (kind: string) => here.changes.filter((c) => c.change === kind).length;
  const parts: string[] = [];
  const changed = of("changed");
  if (changed) parts.push(`${interfaces(changed)} ${changed === 1 ? "changes" : "change"}`);
  if (of("new")) parts.push(`${interfaces(of("new"))} new here`);
  if (of("removed")) parts.push(`${interfaces(of("removed"))} removed`);
  const files =
    here.project_files_change === true
      ? " The project's own files (ductus.yaml, resources.yaml, environments, secrets) change too."
      : "";
  if (parts.length === 0) {
    return `No interface changes: all ${count(here.unchanged)} run here as they are.${files}`;
  }
  return `${parts.join(", ")}; ${count(here.unchanged)} as they run now.${files}`;
}

/**
 * How deploying it here would apply (0.7.5.1): in place, or by the handover and why. Null when the
 * engine didn't say (nothing to compare with, it runs this one) or nothing would change.
 */
export function applySummary(here: Schemas["Here"]): string | null {
  const plan = here.plan;
  if (!plan || here.running) return null;
  if (plan.handover_because.length > 0) {
    return `It needs the handover (a new engine takes over; every interface's sending pauses a moment): ${plan.handover_because.join("; ")}.`;
  }
  if (plan.changes.length === 0) return null;
  // An interface added or removed (0.7.5.2.1) says only that (`what` is ["added"] or ["removed"]).
  const only = (word: string) => plan.changes.filter((c) => c.what.length === 1 && c.what[0] === word).length;
  const [added, removed] = [only("added"), only("removed")];
  // A source changed (0.7.5.2.2) moves, swaps or restarts: not "between two messages".
  const sources = plan.changes.filter((c) => c.what.includes("source")).length;
  const changed = plan.changes.length - added - removed - sources;
  const parts: string[] = [];
  if (changed === 1) parts.push("the interface it changes takes its new code between two messages");
  if (changed > 1) parts.push("the interfaces it changes take their new code between two messages");
  if (sources === 1) parts.push("the source it changes moves, swaps its settings or restarts alone");
  if (sources > 1) parts.push(`the ${count(sources)} sources it changes move, swap their settings or restart alone`);
  if (added) parts.push(added === 1 ? "the one it adds starts" : `the ${count(added)} it adds start`);
  if (removed) parts.push(removed === 1 ? "the one it removes stops" : `the ${count(removed)} it removes stop`);
  return `It applies in place: ${parts.join(", ")}; the others never pause.`;
}

/** One interface's line of the plan: what changes, and "in place" or "the handover: why". */
export function applyOf(here: Schemas["Here"], name: string): { what: string; how: string; inPlace: boolean } | null {
  const c = here.plan?.changes.find((p) => p.interface === name);
  if (!c) return null;
  return {
    what: c.what.join(", "),
    how: c.in_place ? "in place" : `the handover: ${c.handover_because ?? "it needs one"}`,
    inPlace: c.in_place,
  };
}

/** This environment's rules, as sentences (none: a deploy is held only to the gate and the "replaces" check). */
export function rulesText(mode: Schemas["ReleaseMode"], environment: string): string[] {
  const rules: string[] = [];
  if (mode.requires.length > 0) {
    rules.push(`${environment} takes only releases deployed to ${mode.requires.join(" and ")} first.`);
  }
  if (mode.branches.length > 0) {
    rules.push(`${environment} takes only releases made on ${mode.branches.join(" or ")}.`);
  }
  rules.push(
    mode.reuse_check_seconds > 0
      ? `A passing check of the same release is reused for ${duration(mode.reuse_check_seconds)} when nothing was deployed here since.`
      : "The gate runs at every deploy (no check is reused).",
  );
  return rules;
}

/** Why the dialog's check won't spare the deploy its own gate, or null when it will. */
export function reuseCaveat(mode: Schemas["ReleaseMode"]): string | null {
  return mode.reuse_check_seconds === 0
    ? "This engine reuses no check (DUCTUS_DEPLOY_REUSE_CHECK=0): Deploy runs the gate again."
    : null;
}
