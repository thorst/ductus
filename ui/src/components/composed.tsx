import { Link } from "@tanstack/react-router";

/** A release an engine composed for a deploy of some interfaces (`ductus deploy R --interface A`):
 *  which interfaces, from which release (its page linked); everything else is what ran there. */
export function Composed({ from, interfaces }: { from: string; interfaces?: string[] }) {
  const names = interfaces ?? [];
  return (
    <div className="text-xs text-muted">
      Composed: {names.join(", ")} from{" "}
      <Link to="/releases/$id" params={{ id: from.slice(0, 12) }} className="font-mono">
        {from.slice(0, 12)}
      </Link>
      , the rest as it ran
    </div>
  );
}
