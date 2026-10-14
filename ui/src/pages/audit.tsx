import { useInfiniteQuery } from "@tanstack/react-query";
import { getRouteApi, useNavigate } from "@tanstack/react-router";
import { Download } from "lucide-react";
import { useState, type FormEvent } from "react";
import { useCanAudit } from "@/api/actions";
import { audit } from "@/api/queries";
import { ACTIONS, AuditTable } from "@/components/audit-table";
import { Loaded, PageHeader } from "@/components/page";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";

const route = getRouteApi("/app/audit");

/** Whole groups of actions, besides each one. */
const GROUPS: [string, string][] = [
  ["session", "Sign-ins (all)"],
  ["user", "Accounts (all)"],
  ["token", "Tokens (all)"],
  ["message", "Messages (all)"],
];

/** The API's query for a date typed in the filter (local days). */
const dayStart = (day: string) => new Date(`${day}T00:00:00`).toISOString();
const dayAfter = (day: string) => {
  const d = new Date(`${day}T00:00:00`);
  d.setDate(d.getDate() + 1);
  return d.toISOString();
};

/** Who did what, when, from where: for those with the `audit` permission. */
export function AuditPage() {
  const search = route.useSearch();
  const navigate = useNavigate({ from: "/audit" });
  const allowed = useCanAudit();
  const query = useInfiniteQuery({
    ...audit({
      ...search,
      from: search.from ? dayStart(search.from) : undefined,
      to: search.to ? dayAfter(search.to) : undefined,
    }),
    enabled: allowed,
  });
  const entries = query.data?.pages.flatMap((p) => p.entries) ?? [];
  const [form, setForm] = useState({
    actor: search.actor ?? "",
    action: search.action ?? "",
    interface: search.interface ?? "",
    message: search.message ? String(search.message) : "",
    from: search.from ?? "",
    to: search.to ?? "",
  });
  const set = (key: keyof typeof form) => (value: string) => setForm({ ...form, [key]: value });
  const apply = (e: FormEvent) => {
    e.preventDefault();
    const message = Number(form.message);
    void navigate({
      search: {
        actor: form.actor.trim() || undefined,
        action: form.action || undefined,
        interface: form.interface.trim() || undefined,
        message: form.message && Number.isInteger(message) && message > 0 ? message : undefined,
        from: form.from || undefined,
        to: form.to || undefined,
      },
    });
  };
  if (!allowed) {
    return (
      <>
        <PageHeader title="Audit log" />
        <p className="max-w-prose text-sm text-muted">
          Reading the audit log needs the <strong className="text-text">audit</strong> permission. Ask an admin for it.
          What you did yourself is under Account.
        </p>
      </>
    );
  }
  const exportParams = new URLSearchParams({ format: "ndjson" });
  if (search.actor) exportParams.set("actor", search.actor);
  if (search.action) exportParams.set("action", search.action);
  if (search.interface) exportParams.set("interface", search.interface);
  if (search.message) exportParams.set("message", String(search.message));
  if (search.from) exportParams.set("from", dayStart(search.from));
  if (search.to) exportParams.set("to", dayAfter(search.to));
  return (
    <>
      <PageHeader
        title="Audit log"
        actions={
          <a
            href={`/api/v1/audit?${exportParams}`}
            download="audit.ndjson"
            className="inline-flex h-9 items-center gap-2 rounded-md border px-3 text-sm font-medium text-text no-underline hover:bg-hover hover:no-underline"
          >
            <Download aria-hidden className="size-4" />
            Export these (JSON lines)
          </a>
        }
      >
        Who did what, to what, and from where. Kept for six years unless <code>audit.keep</code> says otherwise. The
        export is oldest first, up to 10,000 entries; <code>ductus audit export</code> has them all.
      </PageHeader>
      <form onSubmit={apply} className="mb-4 flex flex-wrap items-end gap-3" aria-label="Filter the audit log">
        <label className="text-sm">
          <span className="block text-xs text-muted">Who</span>
          <Input value={form.actor} onChange={(e) => set("actor")(e.target.value)} className="mt-1 w-40" />
        </label>
        <label className="text-sm">
          <span className="block text-xs text-muted">What</span>
          <select
            className="mt-1 h-9 rounded-md border bg-panel px-2 text-sm"
            value={form.action}
            onChange={(e) => set("action")(e.target.value)}
          >
            <option value="">Anything</option>
            {GROUPS.map(([value, label]) => (
              <option key={value} value={value}>
                {label}
              </option>
            ))}
            {Object.entries(ACTIONS).map(([value, label]) => (
              <option key={value} value={value}>
                {label}
              </option>
            ))}
          </select>
        </label>
        <label className="text-sm">
          <span className="block text-xs text-muted">Interface</span>
          <Input value={form.interface} onChange={(e) => set("interface")(e.target.value)} className="mt-1 w-40" />
        </label>
        <label className="text-sm">
          <span className="block text-xs text-muted">Message id</span>
          <Input
            inputMode="numeric"
            value={form.message}
            onChange={(e) => set("message")(e.target.value)}
            className="mt-1 w-32"
          />
        </label>
        <label className="text-sm">
          <span className="block text-xs text-muted">From</span>
          <Input type="date" value={form.from} onChange={(e) => set("from")(e.target.value)} className="mt-1" />
        </label>
        <label className="text-sm">
          <span className="block text-xs text-muted">Until (including)</span>
          <Input type="date" value={form.to} onChange={(e) => set("to")(e.target.value)} className="mt-1" />
        </label>
        <Button type="submit">Filter</Button>
      </form>
      <Loaded query={query}>
        {() =>
          entries.length === 0 ? (
            <p className="text-sm text-muted">Nothing in the log matches.</p>
          ) : (
            <div className="rounded-lg border bg-panel">
              <AuditTable entries={entries} label="Audit log" />
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
    </>
  );
}
