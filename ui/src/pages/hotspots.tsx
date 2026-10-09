import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { getRouteApi, Link } from "@tanstack/react-router";
import { CircleAlert, CircleCheck, CirclePause } from "lucide-react";
import { useState } from "react";
import type { Schemas } from "@/api/client";
import { hotspots } from "@/api/queries";
import { Loaded, PageHeader } from "@/components/page";
import { Button } from "@/components/ui/button";
import { Card, CardBody, CardHeader, CardTitle } from "@/components/ui/card";
import { Table, Td, Th } from "@/components/ui/table";
import { count, duration } from "@/lib/format";
import { cn } from "@/lib/utils";

const route = getRouteApi("/app/hotspots");

/**
 * Where each backlog sits: the lanes messages wait in (the ordering key's hash), the first
 * message of each, and which worker a failing message holds up. Opened from a stalled or behind
 * destination, or on its own for every destination with messages waiting.
 */
export function HotspotsPage() {
  const search = route.useSearch();
  const [allLanes, setAllLanes] = useState(false);
  const query = useQuery({ ...hotspots(search.interface, allLanes ? 1000 : 20), placeholderData: keepPreviousData });
  const shown = (query.data ?? []).filter((d) => !search.destination || d.destination === search.destination);
  const what = search.interface
    ? `${search.interface}${search.destination && search.destination !== "default" ? ` → ${search.destination}` : ""}`
    : null;
  return (
    <>
      <PageHeader
        title={what ? `Hotspots: ${what}` : "Hotspots"}
        actions={
          <Button variant="ghost" onClick={() => void query.refetch()}>
            Refresh
          </Button>
        }
      >
        Where messages wait, and what's holding them. Each worker sends its lanes one message at a time, in order: a
        message that keeps failing holds up every lane of its worker.
      </PageHeader>
      <Loaded query={query}>
        {() =>
          shown.length === 0 ? (
            <p className="flex items-center gap-2 text-sm text-ok">
              <CircleCheck aria-hidden className="size-5" />
              Nothing waiting{what ? ` for ${what}` : ""}.
            </p>
          ) : (
            <div className="space-y-6">
              {what && (
                <p className="text-sm">
                  <Link to="/hotspots" search={{}}>
                    Every destination
                  </Link>
                </p>
              )}
              {shown.map((d) => (
                <DestinationHotspots
                  key={`${d.interface}/${d.destination}`}
                  d={d}
                  onAllLanes={allLanes ? undefined : () => setAllLanes(true)}
                />
              ))}
            </div>
          )
        }
      </Loaded>
    </>
  );
}

function DestinationHotspots({ d, onAllLanes }: { d: Schemas["DestinationHotspots"]; onAllLanes?: () => void }) {
  const name = d.destination === "default" ? d.interface : `${d.interface} → ${d.destination}`;
  const more = d.findings.filter((f) => !d.summary.includes(f.text));
  return (
    <Card aria-label={name}>
      <CardHeader>
        <CardTitle>
          <Link to="/interfaces/$name" params={{ name: d.interface }}>
            {name}
          </Link>
        </CardTitle>
        <span className="text-xs text-muted">
          {count(d.lanes_total)} {d.lanes_total === 1 ? "lane" : "lanes"} · {count(d.workers_total)}{" "}
          {d.workers_total === 1 ? "worker" : "workers"}
        </span>
      </CardHeader>
      <CardBody className="space-y-4">
        <p
          className={cn("flex gap-2 text-sm", d.findings.length > 0 && "rounded-md border border-bad/40 bg-bad/10 p-3")}
        >
          {d.paused ? (
            <CirclePause aria-hidden className="mt-0.5 size-4 shrink-0 text-paused" />
          ) : (
            d.findings.length > 0 && <CircleAlert aria-hidden className="mt-0.5 size-4 shrink-0 text-bad" />
          )}
          <span>{d.summary}</span>
        </p>
        {/* The summary says the worst finding already; any others are listed. */}
        {more.length > 0 && (
          <ul aria-label="More findings" className="space-y-2">
            {more.map((f, n) => (
              <li key={n} className="flex gap-2 rounded-md border border-bad/40 bg-bad/10 p-3 text-sm">
                <CircleAlert aria-hidden className="mt-0.5 size-4 shrink-0 text-bad" />
                <span>{f.text}</span>
              </li>
            ))}
          </ul>
        )}
        {d.workers.length > 1 && <Workers workers={d.workers} />}
      </CardBody>
      <Lanes d={d} onAllLanes={onAllLanes} />
    </Card>
  );
}

function Workers({ workers }: { workers: Schemas["WorkerInfo"][] }) {
  const most = Math.max(...workers.map((w) => w.waiting));
  return (
    <div>
      <h3 className="mb-2 text-xs font-medium tracking-wide text-muted uppercase">Workers</h3>
      <ul className="space-y-1.5">
        {workers.map((w) => (
          <li key={w.worker} className="grid grid-cols-[6rem_1fr_auto] items-center gap-3 text-sm">
            <span>Worker {w.worker}</span>
            {/* The bar is a picture of the numbers beside it, never the only way to read them. */}
            <span aria-hidden className="h-2 rounded-full bg-hover">
              <span
                className={cn("block h-2 rounded-full", w.blocked_by != null ? "bg-bad" : "bg-accent")}
                style={{ width: `${Math.max(2, (w.waiting / most) * 100)}%` }}
              />
            </span>
            <span className="tabular-nums">
              {count(w.waiting)} in {count(w.lanes_waiting)} {w.lanes_waiting === 1 ? "lane" : "lanes"}
              {w.blocked_by != null && <span className="text-bad"> · blocked by lane {w.blocked_by}</span>}
            </span>
          </li>
        ))}
      </ul>
    </div>
  );
}

function Lanes({ d, onAllLanes }: { d: Schemas["DestinationHotspots"]; onAllLanes?: () => void }) {
  const more = d.lanes_waiting - d.lanes.length;
  return (
    <>
      <Table aria-label={`Lanes waiting for ${d.destination === "default" ? d.interface : d.destination}`}>
        <thead>
          <tr>
            <Th className="text-right">Lane</Th>
            <Th className="text-right">Worker</Th>
            <Th className="text-right">Waiting</Th>
            <Th>First message</Th>
            <Th>Key</Th>
            <Th>Waiting for</Th>
            <Th className="text-right">Attempts</Th>
            <Th>Last error</Th>
          </tr>
        </thead>
        <tbody>
          {/* The engine lists the most waiting first; a lane its worker is retrying comes first here. */}
          {[...d.lanes]
            .sort((a, b) => Number(b.head.attempts > 0) - Number(a.head.attempts > 0))
            .map((l) => (
              <tr key={l.lane} className={cn(l.head.attempts > 0 && "bg-bad/5")}>
                <Td className="text-right tabular-nums">{l.lane}</Td>
                <Td className="text-right tabular-nums">{l.worker ?? <span className="text-bad">none</span>}</Td>
                <Td className="text-right tabular-nums">{count(l.waiting)}</Td>
                <Td>
                  <Link to="/messages/$id" params={{ id: l.head.id }} className="font-mono">
                    {l.head.id}
                  </Link>
                </Td>
                <Td className="font-mono text-xs">{l.head.ordering_key ?? ""}</Td>
                <Td className="tabular-nums">{duration(l.head.waiting_s)}</Td>
                <Td className={cn("text-right tabular-nums", l.head.attempts > 0 && "font-semibold text-bad")}>
                  {l.head.attempts}
                </Td>
                <Td className="max-w-md text-xs break-words">{l.head.last_error ?? ""}</Td>
              </tr>
            ))}
        </tbody>
      </Table>
      {more > 0 && (
        <p className="flex flex-wrap items-center gap-3 px-4 py-3 text-xs text-muted">
          {count(more)} more {more === 1 ? "lane" : "lanes"} with fewer waiting.
          {onAllLanes && (
            <Button size="sm" variant="outline" onClick={onAllLanes}>
              Show every lane
            </Button>
          )}
        </p>
      )}
    </>
  );
}
