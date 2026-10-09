import { useQuery } from "@tanstack/react-query";
import { getRouteApi, Link, useNavigate } from "@tanstack/react-router";
import { ArrowRight, CirclePause, CirclePlay, Flame, MessageSquareText } from "lucide-react";
import { useState } from "react";
import { pause, resume, useCanOperate, useRefresh } from "@/api/actions";
import type { Schemas } from "@/api/client";
import { iface } from "@/api/queries";
import { ActionDialog } from "@/components/action-dialog";
import { HealthBadge } from "@/components/health-badge";
import { MessageList } from "@/components/message-list";
import { Field, Loaded, PageHeader } from "@/components/page";
import { Button } from "@/components/ui/button";
import { Card, CardBody, CardHeader, CardTitle } from "@/components/ui/card";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { ago, count, duration, ms, perMinute } from "@/lib/format";
import { worst } from "@/lib/health";
import { cn } from "@/lib/utils";

const route = getRouteApi("/app/interfaces/$name");

export function InterfacePage() {
  const { name } = route.useParams();
  const { tab, status } = route.useSearch();
  const navigate = useNavigate({ from: "/interfaces/$name" });
  const query = useQuery(iface(name));
  return (
    <Loaded query={query}>
      {(i) => (
        <>
          <PageHeader
            title={
              <span className="flex flex-wrap items-center gap-3">
                {i.name}
                <HealthBadge state={worst(i)} />
              </span>
            }
          >
            {i.description}
          </PageHeader>
          <Tabs
            value={tab ?? "overview"}
            onValueChange={(value) =>
              void navigate({ search: (s) => ({ ...s, tab: value === "messages" ? "messages" : undefined }) })
            }
          >
            <TabsList className="mb-6">
              <TabsTrigger value="overview">Overview</TabsTrigger>
              <TabsTrigger value="messages">Messages</TabsTrigger>
            </TabsList>
            <TabsContent value="overview">
              <Overview i={i} />
            </TabsContent>
            <TabsContent value="messages">
              <MessageList
                name={i.name}
                status={status}
                onStatus={(s) => void navigate({ search: (prev) => ({ ...prev, status: s }) })}
              />
            </TabsContent>
          </Tabs>
        </>
      )}
    </Loaded>
  );
}

function Overview({ i }: { i: Schemas["InterfaceDetail"] }) {
  const system = (s: Schemas["SystemInfo"]) =>
    [s.application, s.vendor && `(${s.vendor})`].filter(Boolean).join(" ") || "–";
  return (
    <div className="space-y-6">
      {i.state === "stopped" && (
        <p role="alert" className="rounded-md border border-bad/40 bg-bad/10 p-4 text-sm">
          <strong className="text-bad">Stopped:</strong> {i.stopped_reason}. Its messages wait until the code is fixed
          and the engine restarted.
        </p>
      )}
      <Card>
        <CardBody>
          <dl className="grid grid-cols-2 gap-x-6 gap-y-4 md:grid-cols-4">
            <Field label="Receives">
              {i.health.state !== "ok" && <HealthBadge state={i.health.state} className="mr-2" />}
              {i.health.summary}
            </Field>
            <Field label="Source">
              <span className="font-mono text-xs">{i.source}</span>
            </Field>
            <Field label="Systems">
              {system(i.from)} <ArrowRight aria-label="to" className="inline size-3.5" /> {system(i.to)}
            </Field>
            <Field label="Owner">{i.owner ?? "–"}</Field>
            <Field label="Format">{i.format}</Field>
            <Field label="Runs">
              {i.isolation === "dedicated" ? `dedicated, ${i.workers} workers` : "shared pool"} · timeout{" "}
              {ms(i.timeout_ms)}
            </Field>
            <Field label="Keeps messages">{i.retention_days ? `${i.retention_days} days` : "–"}</Field>
            <Field label="Received since start">
              {count(i.since_start.received)}
              {i.since_start.duplicates > 0 && ` (+${count(i.since_start.duplicates)} duplicates)`}
            </Field>
            {i.from_hosts.length > 0 && <Field label="Senders connect from">{i.from_hosts.join(", ")}</Field>}
            <Field label="Folder">
              <span className="font-mono text-xs">{i.folder}</span>
            </Field>
          </dl>
        </CardBody>
      </Card>
      <h2 className="text-base font-semibold">Destinations</h2>
      <div className="grid items-start gap-4 xl:grid-cols-2">
        {i.destinations.map((d) => (
          <DestinationCard key={d.name} interfaceName={i.name} d={d} />
        ))}
      </div>
    </div>
  );
}

function DestinationCard({ interfaceName, d }: { interfaceName: string; d: Schemas["DestinationStatus"] }) {
  const h = d.health;
  const c = d.since_start;
  const canOperate = useCanOperate();
  return (
    <Card aria-label={d.name}>
      <CardHeader>
        <div className="min-w-0">
          <CardTitle>{d.name}</CardTitle>
          <div className="truncate font-mono text-xs text-muted">{d.connector}</div>
        </div>
        <HealthBadge state={h.state} />
      </CardHeader>
      {canOperate && <PauseControls interfaceName={interfaceName} d={d} />}
      <CardBody className="space-y-4">
        {/* The engine's summary already says who paused it, when and why. */}
        <p className={cn("text-sm", d.paused && "flex gap-2 rounded-md border border-paused/40 bg-paused/10 p-3")}>
          {d.paused && <CirclePause aria-hidden className="mt-0.5 size-4 shrink-0 text-paused" />}
          {h.summary}
        </p>
        <dl className="grid grid-cols-2 gap-x-6 gap-y-3 sm:grid-cols-4">
          <Field label="Waiting">{count(d.pending)}</Field>
          <Field label="Oldest waiting">{duration(h.oldest_waiting_s)}</Field>
          <Field label="Quarantined">
            {d.quarantined > 0 ? (
              <Link
                to="/quarantine"
                search={{ interface: interfaceName, destination: d.name }}
                className="font-semibold text-bad"
              >
                {count(d.quarantined)}
              </Link>
            ) : (
              count(d.quarantined)
            )}
          </Field>
          <Field label="Catching up in">{h.catch_up_s != null ? `~${duration(h.catch_up_s)}` : "–"}</Field>
          <Field label="Last sent">{ago(h.last_sent)}</Field>
          {/* Rates come after the engine's first half minute; normal after an hour watched. */}
          {h.recent && (
            <Field label="In / out, last minutes">
              {perMinute(h.recent.received_per_min)} / {perMinute(h.recent.done_per_min)}
            </Field>
          )}
          {h.normal.received_per_min != null && <Field label="Normal in">{perMinute(h.normal.received_per_min)}</Field>}
          {(h.recent?.send_ms != null || h.normal.send_ms != null) && (
            <Field label="Send now / normal">
              {ms(h.recent?.send_ms)} / {ms(h.normal.send_ms)}
            </Field>
          )}
        </dl>
        {h.last_error && (
          <div className="rounded-md border bg-bg p-3 text-sm">
            <div className="text-xs text-muted">Last error, {ago(h.last_error.at)}</div>
            <div className="mt-1 font-mono text-xs break-words">{h.last_error.error}</div>
          </div>
        )}
        {(h.state === "stalled" || h.state === "behind") && (
          <Link
            to="/hotspots"
            search={{ interface: interfaceName, destination: d.name }}
            className="inline-flex items-center gap-1.5 text-sm font-medium"
          >
            <Flame aria-hidden className="size-4" />
            Where it's stuck
          </Link>
        )}
        <details className="text-sm">
          <summary className="cursor-pointer text-muted">Since the engine started</summary>
          <dl className="mt-3 grid grid-cols-2 gap-x-6 gap-y-3 sm:grid-cols-4">
            <Field label="Sent">{count(c.sent)}</Field>
            <Field label="Filtered">{count(c.filtered)}</Field>
            <Field label="Quarantined">{count(c.quarantined)}</Field>
            <Field label="Retries">{count(c.retries)}</Field>
            <Field label="Receipt → sent p50 / p95">
              {ms(c.latency.p50_ms)} / {ms(c.latency.p95_ms)}
            </Field>
            <Field label="Send p50 / p95">
              {ms(c.send.p50_ms)} / {ms(c.send.p95_ms)}
            </Field>
            <Field label="Transform p50 / p95">
              {ms(c.transform.p50_ms)} / {ms(c.transform.p95_ms)}
            </Field>
          </dl>
        </details>
      </CardBody>
    </Card>
  );
}

/**
 * Pause at once (a reason is optional and can come later: act first, explain after), add the
 * reason, or resume (the backlog goes in order).
 */
function PauseControls({ interfaceName, d }: { interfaceName: string; d: Schemas["DestinationStatus"] }) {
  const [dialog, setDialog] = useState<"pause" | "reason" | "resume" | null>(null);
  const refresh = useRefresh();
  const close = (open: boolean) => !open && setDialog(null);
  return (
    <div className="flex flex-wrap gap-2 border-b px-4 py-2">
      {d.paused ? (
        <>
          <Button size="sm" variant="outline" onClick={() => setDialog("resume")}>
            <CirclePlay aria-hidden />
            Resume<span className="sr-only"> {d.name}</span>
          </Button>
          {!d.paused.reason && (
            <Button size="sm" variant="ghost" onClick={() => setDialog("reason")}>
              <MessageSquareText aria-hidden />
              Add the reason
            </Button>
          )}
        </>
      ) : (
        <Button size="sm" variant="outline" onClick={() => setDialog("pause")}>
          <CirclePause aria-hidden />
          Pause<span className="sr-only"> {d.name}</span>
        </Button>
      )}
      <ActionDialog
        open={dialog === "pause" || dialog === "reason"}
        onOpenChange={close}
        title={dialog === "reason" ? `Why is ${d.name} paused?` : `Pause ${d.name}`}
        description={
          dialog === "reason"
            ? "Kept with the pause, so everyone sees it."
            : "Nothing is sent or retried until it's resumed (a send under way finishes). Messages keep queueing in order. The pause holds across restarts; a drain doesn't wait for it."
        }
        confirm={dialog === "reason" ? "Save the reason" : "Pause"}
        onConfirm={async (why) => {
          await pause(interfaceName, d.name, why);
          await refresh();
        }}
      />
      <ActionDialog
        open={dialog === "resume"}
        onOpenChange={close}
        reason={false}
        title={`Resume ${d.name}`}
        description={`It sends the ${count(d.pending)} waiting, in order, then carries on.`}
        confirm="Resume"
        onConfirm={async () => {
          await resume(interfaceName, d.name);
          await refresh();
        }}
      />
    </div>
  );
}
