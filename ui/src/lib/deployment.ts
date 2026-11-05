import type { Schemas } from "@/api/client";
import { count } from "@/lib/format";

/** How many hex characters of a git commit are shown, as for code versions. */
const SHOWN = 12;

/**
 * What the engine's start recorded, after "deployed {time}": "by todd (3 interfaces changed):
 * fix PV1-3 · commit 3f9c2e81b07a, 2 files not committed".
 */
export function deployedBy(d: Schemas["DeploymentInfo"]): string {
  const changed =
    d.changed === 0
      ? "nothing changed"
      : d.changed === 1
        ? "1 interface changed"
        : `${count(d.changed)} interfaces changed`;
  let text = `by ${d.by} (${changed})`;
  if (d.reason) text += `: ${d.reason}`;
  if (d.git_commit) text += ` · commit ${d.git_commit.slice(0, SHOWN)}`;
  const uncommitted = d.uncommitted?.length ?? 0;
  if (uncommitted === 1) text += ", 1 file not committed";
  if (uncommitted > 1) text += `, ${count(uncommitted)} files not committed`;
  return text;
}
