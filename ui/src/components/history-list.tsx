import { useInfiniteQuery } from "@tanstack/react-query";
import { FileText, GitCommitHorizontal, History, Rocket, TriangleAlert } from "lucide-react";
import { useState } from "react";
import type { Schemas } from "@/api/client";
import { history } from "@/api/queries";
import { Loaded } from "@/components/page";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { timestamp } from "@/lib/format";

type Entry = Schemas["HistoryEntry"];

const KINDS: Record<Entry["kind"], { label: string; icon: typeof History }> = {
  commit: { label: "Commit", icon: GitCommitHorizontal },
  deployment: { label: "Deployed", icon: Rocket },
  audit: { label: "Audit log", icon: History },
  documentation: { label: "Documentation", icon: FileText },
};

/**
 * One timeline of who changed the interface, what and why, newest first: git commits on its files, the deployments
 * that changed its code, the audit log's entries naming it and its documentation's changes. "Everything" adds the
 * starts that ran it unchanged (and, for those who may read the audit log, payload reads and refused attempts).
 */
export function HistoryList({ name }: { name: string }) {
  const [all, setAll] = useState(false);
  const query = useInfiniteQuery(history(name, all));
  const entries = query.data?.pages.flatMap((p) => p.entries) ?? [];
  const notes = query.data?.pages[0]?.notes ?? [];
  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center gap-4">
        <label className="flex items-center gap-2 text-sm">
          <input type="checkbox" checked={all} onChange={(e) => setAll(e.target.checked)} />
          Everything (also starts that changed nothing, reads and refused attempts)
        </label>
        <Button size="sm" variant="outline" onClick={() => void query.refetch()}>
          Refresh
        </Button>
      </div>
      {notes.map((n) => (
        <p key={n} className="text-sm text-muted">
          {n}
        </p>
      ))}
      <Loaded query={query}>
        {() =>
          entries.length === 0 ? (
            <p className="text-sm text-muted">Nothing recorded yet.</p>
          ) : (
            <Card aria-label="History">
              <ol className="divide-y">
                {entries.map((e) => (
                  <HistoryItem key={e.key} entry={e} />
                ))}
              </ol>
            </Card>
          )
        }
      </Loaded>
      {query.hasNextPage && (
        <Button
          size="sm"
          variant="outline"
          disabled={query.isFetchingNextPage}
          onClick={() => void query.fetchNextPage()}
        >
          {query.isFetchingNextPage ? "Loading…" : "Older"}
        </Button>
      )}
    </div>
  );
}

function HistoryItem({ entry: e }: { entry: Entry }) {
  const kind = KINDS[e.kind];
  const Icon = kind.icon;
  const warning = e.deployment?.warning;
  return (
    <li className="grid grid-cols-[1.25rem_1fr] gap-x-3 px-4 py-3 text-sm">
      <Icon aria-hidden className="mt-0.5 size-4 text-muted" />
      <div className="min-w-0 space-y-1">
        <div className="flex flex-wrap items-baseline gap-x-3 text-xs text-muted">
          <time dateTime={e.at}>{timestamp(e.at)}</time>
          <span>{kind.label}</span>
          {e.by && <span>{e.by}</span>}
        </div>
        <p className="wrap-anywhere">{e.what}</p>
        {e.commit && e.commit.files.length > 0 && (
          <ul className="font-mono text-xs break-all text-muted">
            {e.commit.files.map((f) => (
              <li key={f.path}>
                {f.change} {f.path}
              </li>
            ))}
          </ul>
        )}
        {e.documentation && e.documentation.fields.length > 0 && (
          <ul className="text-xs text-muted">
            {e.documentation.fields.map((f) => (
              <li key={f.field}>
                <span className="font-mono">{f.field}</span>: {shown(f.before)} → {shown(f.after)}
              </li>
            ))}
          </ul>
        )}
        {warning && (
          <p role="note" className="flex gap-2 rounded-md border border-warn/40 bg-warn/10 p-2 text-xs">
            <TriangleAlert aria-hidden className="mt-0.5 size-3.5 shrink-0 text-warn" />
            <span className="wrap-anywhere">{warning}</span>
          </p>
        )}
      </div>
    </li>
  );
}

/** A documentation value in a line: text as it is, – for none. */
function shown(value: unknown): string {
  if (value === null || value === undefined) return "–";
  return typeof value === "string" ? value : JSON.stringify(value);
}
