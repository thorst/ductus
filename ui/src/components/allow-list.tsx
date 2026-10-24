import { ShieldCheck, TriangleAlert } from "lucide-react";
import type { Schemas } from "@/api/client";
import { ago, count } from "@/lib/format";

/**
 * Who may connect to a listener (`allow:`): the list, each host name with the addresses it
 * resolves to now (a name that doesn't resolve lets no one in), and what was refused since start.
 */
export function AllowList({ allow }: { allow: Schemas["SourceAllow"] }) {
  const names = new Map(allow.names.map((n) => [n.name, n]));
  return (
    <span className="block text-xs">
      <span className="inline-flex items-start gap-1">
        <ShieldCheck aria-hidden className="mt-px size-3.5 shrink-0 text-ok" />
        <span>
          Only{" "}
          {allow.entries.map((entry, n) => {
            const name = names.get(entry.toLowerCase());
            return (
              <span key={entry}>
                {n > 0 && ", "}
                <span className="font-mono">{entry}</span>
                {name && name.addresses.length > 0 && (
                  <span className="text-muted"> ({name.addresses.join(", ")})</span>
                )}
                {name?.error && (
                  <span className="text-warn">
                    {" "}
                    <TriangleAlert aria-hidden className="inline size-3 align-[-1px]" />{" "}
                    {name.addresses.length > 0
                      ? `didn't resolve ${ago(name.checked_at)}; still let in`
                      : "doesn't resolve: no one is let in for it"}
                  </span>
                )}
              </span>
            );
          })}{" "}
          may connect
        </span>
      </span>
      {allow.refused > 0 && (
        <span className="block text-muted">
          {count(allow.refused)} refused since start, last from <span className="font-mono">{allow.last_refused}</span>{" "}
          {ago(allow.last_refused_at)}
        </span>
      )}
    </span>
  );
}
