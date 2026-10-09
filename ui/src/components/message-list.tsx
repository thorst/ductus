import { useInfiniteQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { useVirtualizer } from "@tanstack/react-virtual";
import { useEffect, useRef } from "react";
import { messages, STATUSES, type MessageStatus } from "@/api/queries";
import { StatusBadge } from "@/components/health-badge";
import { Loaded } from "@/components/page";
import { Button } from "@/components/ui/button";
import { bytes, timestamp } from "@/lib/format";
import { cn } from "@/lib/utils";

const ROW = 41;
const COLUMNS = "grid grid-cols-[6rem_11rem_7rem_minmax(7rem,1fr)_minmax(7rem,1fr)_5rem_minmax(10rem,2fr)]";

/**
 * An interface's messages, newest first. Older pages load as you scroll; only the rows on
 * screen are drawn, so a long history stays fast.
 */
export function MessageList({
  name,
  status,
  onStatus,
}: {
  name: string;
  status?: MessageStatus;
  onStatus: (status?: MessageStatus) => void;
}) {
  const query = useInfiniteQuery(messages(name, status));
  const rows = query.data?.pages.flatMap((p) => p.messages) ?? [];
  const scroller = useRef<HTMLDivElement>(null);
  // TanStack Virtual's API returns functions the React compiler can't memoize; that's expected.
  // eslint-disable-next-line react-hooks/incompatible-library
  const virtual = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scroller.current,
    estimateSize: () => ROW,
    overscan: 10,
  });
  const items = virtual.getVirtualItems();
  const last = items[items.length - 1]?.index ?? 0;
  const { hasNextPage, isFetchingNextPage, fetchNextPage } = query;
  useEffect(() => {
    if (hasNextPage && !isFetchingNextPage && last >= rows.length - 20) void fetchNextPage();
  }, [last, rows.length, hasNextPage, isFetchingNextPage, fetchNextPage]);

  return (
    <div className="space-y-4">
      <div role="group" aria-label="Status" className="flex flex-wrap gap-2">
        {[undefined, ...STATUSES].map((s) => (
          <Button
            key={s ?? "all"}
            size="sm"
            variant={s === status ? "primary" : "outline"}
            aria-pressed={s === status}
            onClick={() => onStatus(s)}
          >
            {s ?? "all"}
          </Button>
        ))}
        <Button size="sm" variant="ghost" className="ml-auto" onClick={() => void query.refetch()}>
          Refresh
        </Button>
      </div>
      <Loaded query={query}>
        {() =>
          rows.length === 0 ? (
            <p className="text-sm text-muted">No {status ?? ""} messages.</p>
          ) : (
            // Narrow screens scroll the whole table sideways, header and rows together.
            <div className="overflow-x-auto rounded-lg border bg-panel">
              <div role="table" aria-label={`${name} messages`} aria-rowcount={rows.length} className="min-w-[60rem]">
                <div role="rowgroup">
                  <div
                    role="row"
                    className={cn(COLUMNS, "border-b text-xs font-medium tracking-wide text-muted uppercase")}
                  >
                    {["Id", "Received", "Status", "Control id", "Key", "Size", "Error"].map((h) => (
                      <div role="columnheader" key={h} className="px-3 py-2">
                        {h}
                      </div>
                    ))}
                  </div>
                </div>
                <div ref={scroller} role="rowgroup" className="h-[calc(100vh-22rem)] min-h-80 overflow-auto">
                  <div style={{ height: virtual.getTotalSize(), position: "relative" }}>
                    {items.map((item) => {
                      const m = rows[item.index]!;
                      return (
                        <div
                          role="row"
                          aria-rowindex={item.index + 1}
                          key={m.id}
                          className={cn(COLUMNS, "absolute inset-x-0 items-center border-b text-sm hover:bg-hover")}
                          style={{ height: ROW, transform: `translateY(${item.start}px)` }}
                        >
                          <div role="cell" className="px-3">
                            <Link to="/messages/$id" params={{ id: m.id }} className="font-mono">
                              {m.id}
                            </Link>
                          </div>
                          <div role="cell" className="px-3 tabular-nums">
                            {timestamp(m.received_at)}
                          </div>
                          <div role="cell" className="px-3">
                            <StatusBadge status={m.status} />
                          </div>
                          <div role="cell" className="truncate px-3 font-mono text-xs">
                            {m.control_id ?? ""}
                          </div>
                          <div role="cell" className="truncate px-3 font-mono text-xs">
                            {m.ordering_key ?? ""}
                          </div>
                          <div role="cell" className="px-3 text-xs tabular-nums">
                            {bytes(m.size)}
                          </div>
                          <div
                            role="cell"
                            className="truncate px-3 text-xs text-muted"
                            title={m.last_error ?? undefined}
                          >
                            {m.last_error ?? ""}
                          </div>
                        </div>
                      );
                    })}
                  </div>
                  {isFetchingNextPage && <p className="p-3 text-sm text-muted">Loading older messages…</p>}
                </div>
              </div>
            </div>
          )
        }
      </Loaded>
    </div>
  );
}
