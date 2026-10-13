import { useQuery } from "@tanstack/react-query";
import { getRouteApi, Link, useNavigate } from "@tanstack/react-router";
import { Search } from "lucide-react";
import { useMemo, useState } from "react";
import type { Schemas } from "@/api/client";
import { docs, interfaces } from "@/api/queries";
import { DocsFilter } from "@/components/docs-values";
import { HealthBadge } from "@/components/health-badge";
import { Loaded, PageHeader } from "@/components/page";
import { Input } from "@/components/ui/input";
import { Table, Td, Th } from "@/components/ui/table";
import { entryNamed, matches } from "@/lib/docs";
import { ago, count } from "@/lib/format";
import { describe, worst } from "@/lib/health";
import { cn } from "@/lib/utils";

type Row = Schemas["InterfaceStatus"] & { worst: string; waiting: number; quarantined: number };

const route = getRouteApi("/app/interfaces");

export function InterfacesPage() {
  const query = useQuery(interfaces);
  const { data: report } = useQuery(docs);
  const { field, value } = route.useSearch();
  const navigate = useNavigate({ from: "/interfaces" });
  const [text, setText] = useState("");
  const [onlyProblems, setOnlyProblems] = useState(false);
  const rows = useMemo(() => {
    const words = text.toLowerCase().split(/\s+/).filter(Boolean);
    return (query.data ?? [])
      .map((i): Row => ({
        ...i,
        worst: worst(i),
        waiting: i.destinations.reduce((n, d) => n + d.pending, 0),
        quarantined: i.destinations.reduce((n, d) => n + d.quarantined, 0),
      }))
      .filter((i) => !onlyProblems || describe(i.worst).rank >= 2)
      .filter((i) => {
        const chosen = report?.fields.find((f) => f.key === field);
        const entry = report && entryNamed(report, "interface", i.name);
        return !chosen || !value || (entry !== undefined && matches(entry, chosen, value));
      })
      .filter((i) => {
        const haystack = [i.name, i.description, i.owner, i.source, ...i.destinations.map((d) => d.connector)]
          .join(" ")
          .toLowerCase();
        return words.every((w) => haystack.includes(w));
      })
      .sort((a, b) => describe(b.worst).rank - describe(a.worst).rank || a.name.localeCompare(b.name));
  }, [query.data, text, onlyProblems, report, field, value]);

  return (
    <>
      <PageHeader title="Interfaces">Worst first; then by name.</PageHeader>
      <div className="mb-4 flex flex-wrap items-center gap-4">
        <div className="relative w-full max-w-sm">
          <Search aria-hidden className="pointer-events-none absolute top-2.5 left-3 size-4 text-muted" />
          <Input
            type="search"
            aria-label="Filter interfaces"
            placeholder="Filter by name, owner, host, port…"
            className="pl-9"
            value={text}
            onChange={(e) => setText(e.target.value)}
          />
        </div>
        <label className="flex items-center gap-2 text-sm">
          <input
            type="checkbox"
            className="size-4 accent-(--accent)"
            checked={onlyProblems}
            onChange={(e) => setOnlyProblems(e.target.checked)}
          />
          Only those needing attention
        </label>
        {report && report.fields.some((f) => f.applies_to.includes("interface")) && (
          <DocsFilter
            report={{ ...report, fields: report.fields.filter((f) => f.applies_to.includes("interface")) }}
            field={field}
            value={value}
            onChange={(f, v) => void navigate({ search: { field: f, value: v } })}
          />
        )}
      </div>
      <Loaded query={query}>{() => <InterfaceTable rows={rows} />}</Loaded>
    </>
  );
}

function InterfaceTable({ rows }: { rows: Row[] }) {
  const navigate = useNavigate();
  if (rows.length === 0) return <p className="text-sm text-muted">No interface matches.</p>;
  return (
    <div className="rounded-lg border bg-panel">
      <Table>
        <thead>
          <tr>
            <Th>Interface</Th>
            <Th>Health</Th>
            <Th>Source</Th>
            <Th className="text-right">Waiting</Th>
            <Th className="text-right">Quarantined</Th>
            <Th>Last received</Th>
            <Th>Owner</Th>
          </tr>
        </thead>
        <tbody>
          {rows.map((i) => (
            <tr
              key={i.name}
              className="cursor-pointer hover:bg-hover"
              onClick={(e) => {
                if (!(e.target instanceof HTMLAnchorElement))
                  void navigate({ to: "/interfaces/$name", params: { name: i.name } });
              }}
            >
              <Td>
                <Link to="/interfaces/$name" params={{ name: i.name }} className="font-medium">
                  {i.name}
                </Link>
                {i.description && <div className="max-w-xs truncate text-xs text-muted">{i.description}</div>}
              </Td>
              <Td>
                <HealthBadge state={i.worst} />
              </Td>
              <Td className="font-mono text-xs">{i.source}</Td>
              <Td className="text-right tabular-nums">{count(i.waiting)}</Td>
              <Td className={cn("text-right tabular-nums", i.quarantined > 0 && "font-semibold text-bad")}>
                {count(i.quarantined)}
              </Td>
              <Td className="whitespace-nowrap">{ago(i.health.last_received)}</Td>
              <Td>{i.owner ?? <span className="text-muted">–</span>}</Td>
            </tr>
          ))}
        </tbody>
      </Table>
    </div>
  );
}
