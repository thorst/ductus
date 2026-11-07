import type { Schemas } from "@/api/client";
import type { HistoryPart } from "@/api/queries";

type Entry = Schemas["HistoryEntry"];

/**
 * The parts a history entry touches, in words: "source, lab". Empty when it's the whole interface's only (its owner,
 * its retention), and for a deployment that brought it here new (every part, which says nothing).
 */
export function touched(e: Entry): string {
  const t = e.touches;
  if (t.everything) return e.kind === "deployment" ? "" : "every part";
  const named = [...(t.source ? ["source"] : []), ...t.destinations];
  if (t.unsure) named.push("code of parts not recorded (before 0.7.4.2)");
  return named.join(", ");
}

/**
 * Whether an entry is known to touch the part shown. An entry that only may (its code changed before 0.7.4.2 recorded
 * which files each destination's code loads) is shown under every part, marked.
 */
export function surely(e: Entry, part: HistoryPart): boolean {
  const t = e.touches;
  if (t.everything || part.kind === "whole") return true;
  return part.kind === "source" ? t.source : t.destinations.includes(part.name);
}

/** The picker's value for a part: "" the whole interface, "source", or "destination:<name>". */
export function encodePart(part: HistoryPart): string {
  return part.kind === "whole" ? "" : part.kind === "source" ? "source" : `destination:${part.name}`;
}

export function decodePart(value: string): HistoryPart {
  if (value === "source") return { kind: "source" };
  if (value.startsWith("destination:")) return { kind: "destination", name: value.slice("destination:".length) };
  return { kind: "whole" };
}
