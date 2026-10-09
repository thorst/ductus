import { useInfiniteQuery, useQuery } from "@tanstack/react-query";
import { getRouteApi, Link, useNavigate } from "@tanstack/react-router";
import { RotateCcw } from "lucide-react";
import { useState } from "react";
import { resendQuarantined, useCanOperate, useRefresh, type Picked } from "@/api/actions";
import { interfaces, quarantine } from "@/api/queries";
import { ActionDialog } from "@/components/action-dialog";
import { Loaded, PageHeader } from "@/components/page";
import { Button } from "@/components/ui/button";
import { Table, Td, Th } from "@/components/ui/table";
import { count, timestamp } from "@/lib/format";

const route = getRouteApi("/app/quarantine");

const key = (p: Picked) => `${p.id}.${p.destination}`;

/**
 * Everything quarantined, across interfaces: what needs a person. Pick deliveries and resend
 * them (after fixing the cause), or open one to correct it.
 */
export function QuarantinePage() {
  const search = route.useSearch();
  const navigate = useNavigate({ from: "/quarantine" });
  const all = useQuery(interfaces);
  const query = useInfiniteQuery(quarantine(search));
  const rows = query.data?.pages.flatMap((p) => p.deliveries) ?? [];
  const canOperate = useCanOperate();
  const refresh = useRefresh();
  const [picked, setPicked] = useState<Map<string, Picked>>(new Map());
  const [confirming, setConfirming] = useState(false);
  const [done, setDone] = useState<string | null>(null);

  // What's picked and still listed (a row resolved elsewhere drops out by itself).
  const listed = new Set(rows.map(key));
  const chosen = [...picked.values()].filter((p) => listed.has(key(p)));
  const allPicked = rows.length > 0 && chosen.length === rows.length;
  const toggle = (p: Picked, on: boolean) => {
    const next = new Map(picked);
    if (on) next.set(key(p), p);
    else next.delete(key(p));
    setPicked(next);
  };

  const withQuarantine = (all.data ?? []).filter((i) => i.destinations.some((d) => d.quarantined > 0));
  const total = (all.data ?? []).flatMap((i) => i.destinations).reduce((n, d) => n + d.quarantined, 0);
  const destinations = [
    ...new Set(
      (all.data ?? [])
        .filter((i) => !search.interface || i.name === search.interface)
        .flatMap((i) => i.destinations.filter((d) => d.quarantined > 0).map((d) => d.name)),
    ),
  ];
  const filter = (next: { interface?: string; destination?: string }) => {
    setPicked(new Map());
    void navigate({ search: next });
  };

  return (
    <>
      <PageHeader title="Quarantine">
        {count(total)} {total === 1 ? "delivery needs" : "deliveries need"} a person: fix the cause, then resend, or
        open a message to correct it.
      </PageHeader>
      <div className="space-y-4">
        <div className="flex flex-wrap items-end gap-3">
          <label className="text-sm">
            <span className="block text-xs text-muted">Interface</span>
            <select
              className="mt-1 h-9 rounded-md border bg-panel px-2 text-sm"
              value={search.interface ?? ""}
              onChange={(e) => filter({ interface: e.target.value || undefined })}
            >
              <option value="">All</option>
              {withQuarantine.map((i) => (
                <option key={i.name} value={i.name}>
                  {i.name} ({count(i.destinations.reduce((n, d) => n + d.quarantined, 0))})
                </option>
              ))}
            </select>
          </label>
          <label className="text-sm">
            <span className="block text-xs text-muted">Destination</span>
            <select
              className="mt-1 h-9 rounded-md border bg-panel px-2 text-sm"
              value={search.destination ?? ""}
              onChange={(e) => filter({ interface: search.interface, destination: e.target.value || undefined })}
            >
              <option value="">All</option>
              {destinations.map((d) => (
                <option key={d} value={d}>
                  {d}
                </option>
              ))}
            </select>
          </label>
          <div className="ml-auto flex items-center gap-2">
            {canOperate && (
              <Button disabled={chosen.length === 0} onClick={() => setConfirming(true)}>
                <RotateCcw aria-hidden />
                Resend {chosen.length > 0 ? count(chosen.length) : ""} selected
              </Button>
            )}
            <Button variant="ghost" onClick={() => void query.refetch()}>
              Refresh
            </Button>
          </div>
        </div>
        {done && (
          <p role="status" className="rounded-md border border-ok/40 bg-ok/10 p-3 text-sm">
            {done}
          </p>
        )}
        <Loaded query={query}>
          {() =>
            rows.length === 0 ? (
              <p className="text-sm text-muted">
                Nothing quarantined{search.interface ? ` in ${search.interface}` : ""}.
              </p>
            ) : (
              <div className="rounded-lg border bg-panel">
                <Table aria-label="Quarantined deliveries">
                  <thead>
                    <tr>
                      {canOperate && (
                        <Th className="w-10">
                          <input
                            type="checkbox"
                            aria-label="Select all listed"
                            checked={allPicked}
                            onChange={(e) =>
                              setPicked(
                                e.target.checked
                                  ? new Map(rows.map((r) => [key(r), { id: r.id, destination: r.destination }]))
                                  : new Map(),
                              )
                            }
                          />
                        </Th>
                      )}
                      <Th>Message</Th>
                      <Th>Interface</Th>
                      <Th>Destination</Th>
                      <Th>Why</Th>
                      <Th className="text-right">Attempts</Th>
                      <Th>Quarantined</Th>
                      <Th>Control id</Th>
                      <Th>Key</Th>
                    </tr>
                  </thead>
                  <tbody>
                    {rows.map((r) => (
                      <tr key={key(r)} className="hover:bg-hover">
                        {canOperate && (
                          <Td>
                            <input
                              type="checkbox"
                              aria-label={`Select message ${r.id} for ${r.destination}`}
                              checked={picked.has(key(r))}
                              onChange={(e) => toggle({ id: r.id, destination: r.destination }, e.target.checked)}
                            />
                          </Td>
                        )}
                        <Td>
                          <Link to="/messages/$id" params={{ id: r.id }} className="font-mono">
                            {r.id}
                          </Link>
                        </Td>
                        <Td>{r.interface}</Td>
                        <Td>{r.destination}</Td>
                        <Td className="max-w-md text-xs break-words">{r.last_error ?? ""}</Td>
                        <Td className="text-right tabular-nums">{r.attempts}</Td>
                        <Td className="tabular-nums">{timestamp(r.quarantined_at)}</Td>
                        <Td className="font-mono text-xs">{r.control_id ?? ""}</Td>
                        <Td className="font-mono text-xs">{r.ordering_key ?? ""}</Td>
                      </tr>
                    ))}
                  </tbody>
                </Table>
                {query.hasNextPage && (
                  <div className="p-3">
                    <Button
                      variant="outline"
                      size="sm"
                      disabled={query.isFetchingNextPage}
                      onClick={() => void query.fetchNextPage()}
                    >
                      {query.isFetchingNextPage ? "Loading…" : "Load more"}
                    </Button>
                  </div>
                )}
              </div>
            )
          }
        </Loaded>
      </div>
      <ActionDialog
        open={confirming}
        onOpenChange={setConfirming}
        title={`Resend ${count(chosen.length)} quarantined ${chosen.length === 1 ? "delivery" : "deliveries"}`}
        description="Each goes back in its destination's queue in its place, sent next for its key through today's code. One resolved by someone else meanwhile is skipped."
        confirm="Resend"
        onConfirm={async (why) => {
          const result = await resendQuarantined(chosen, why);
          const resent = result.resent.reduce((n, m) => n + m.destinations.length, 0);
          setDone(
            `Resent ${count(resent)} ${resent === 1 ? "delivery" : "deliveries"}` +
              (result.skipped.length ? `; ${count(result.skipped.length)} skipped (no longer quarantined).` : "."),
          );
          setPicked(new Map());
          await refresh();
        }}
      />
    </>
  );
}
