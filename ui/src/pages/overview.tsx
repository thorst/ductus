import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { CircleCheck, Server } from "lucide-react";
import type { Schemas } from "@/api/client";
import { certificates, engine, interfaces } from "@/api/queries";
import { AlertItem, useOpenAlerts } from "@/components/alerts";
import { CertificateDays } from "@/components/certificates";
import { HealthBadge } from "@/components/health-badge";
import { Loaded, PageHeader } from "@/components/page";
import { Card, CardBody, CardHeader, CardTitle } from "@/components/ui/card";
import { Table, Td, Th } from "@/components/ui/table";
import { count, duration, ms, timestamp } from "@/lib/format";
import { needingRenewal } from "@/lib/certificates";
import { describe, problems } from "@/lib/health";
import { cn } from "@/lib/utils";

export function OverviewPage() {
  const info = useQuery(engine);
  const list = useQuery(interfaces);
  return (
    <>
      <PageHeader title="Overview">
        {info.data && (
          <>
            {count(info.data.interfaces)} interfaces · up {duration(info.data.uptime_seconds)}
          </>
        )}
      </PageHeader>
      <Loaded query={list}>
        {(all) => (
          <div className="space-y-6">
            {info.data && <DrainBanner drain={info.data.drain} />}
            {info.data && !info.data.holds_interfaces && <LostLockBanner />}
            <OpenAlerts />
            {info.data && <OtherEngines others={info.data.others} />}
            <ExpiringCertificates />
            <Totals all={all} />
            <Attention all={all} />
            {info.data && <Resources resources={info.data.resources.filter((r) => r.calls.count + r.errors > 0)} />}
          </div>
        )}
      </Loaded>
    </>
  );
}

function DrainBanner({ drain }: { drain: Schemas["DrainStatus"] }) {
  if (drain.state === "accepting") return null;
  const drained = drain.state === "drained";
  return (
    <p role="status" className="rounded-md border border-warn/40 bg-warn/10 p-4 text-sm">
      <strong>{drained ? "Drained:" : "Draining:"}</strong>{" "}
      {drained
        ? "no new messages are taken, and everything stored has been delivered."
        : `no new messages are taken; ${count(drain.pending)} deliveries still to go` +
          (drain.waiting_for.length ? ` (${drain.waiting_for.join(", ")})` : "") +
          "."}
      {drain.stop_when_drained && " The engine stops once drained."}
    </p>
  );
}

/** The engine lost the database connection that holds its interfaces (a restart or failover). */
function LostLockBanner() {
  return (
    <p role="status" className="rounded-md border border-bad/40 bg-bad/10 p-4 text-sm">
      <strong>Cut off from the database:</strong> this engine lost the connection that holds its interfaces. Sending,
      file pickup and schedules wait until it is back; receiving goes on.
    </p>
  );
}

/**
 * Other engines on this database and what each runs (only when there are any). An interface runs
 * on one engine at a time: a second engine with the same interface refuses to start.
 */
function OtherEngines({ others }: { others: Schemas["OtherEngine"][] }) {
  if (others.length === 0) return null;
  return (
    <Card aria-label="Other engines on this database">
      <CardHeader>
        <CardTitle>Other engines on this database</CardTitle>
      </CardHeader>
      <ul className="divide-y">
        {others.map((e) => (
          <li key={`${e.host}/${e.pid}`} className="flex flex-wrap items-baseline gap-x-3 gap-y-1 px-4 py-3 text-sm">
            <Server aria-hidden className="size-4 shrink-0 self-center text-muted" />
            <span className="font-medium">{e.host}</span>
            <span className="text-muted">
              Ductus {e.version} · {e.environment} · since {timestamp(e.started_at)} · pid {e.pid}
            </span>
            <span className="basis-full text-muted">
              {e.interfaces.length === 0
                ? "Runs no queued interfaces."
                : `Runs ${e.interfaces.length === 1 ? "" : `${count(e.interfaces.length)} interfaces: `}${e.interfaces.slice(0, 8).join(", ")}${e.interfaces.length > 8 ? ` and ${count(e.interfaces.length - 8)} more` : ""}`}
            </span>
          </li>
        ))}
      </ul>
    </Card>
  );
}

/** Open alerts, worst first (only when alerts are set up and something is open). */
function OpenAlerts() {
  const { open } = useOpenAlerts();
  if (open.length === 0) return null;
  return (
    <Card aria-label="Open alerts">
      <CardHeader>
        <CardTitle>Open alerts</CardTitle>
        <Link to="/alerts" className="text-sm">
          All alerts
        </Link>
      </CardHeader>
      <ul className="divide-y">
        {open.slice(0, 5).map((a) => (
          <AlertItem key={a.id} alert={a} />
        ))}
      </ul>
      {open.length > 5 && <CardBody className="text-sm text-muted">and {count(open.length - 5)} more</CardBody>}
    </Card>
  );
}

/** Certificates expired or within the first warning step (only when there are any). */
function ExpiringCertificates() {
  const { data } = useQuery(certificates);
  const expiring = needingRenewal(data?.certificates ?? []);
  if (expiring.length === 0) return null;
  return (
    <Card aria-label="Certificates expiring">
      <CardHeader>
        <CardTitle>Certificates expiring</CardTitle>
        <Link to="/connections" className="text-sm">
          All certificates
        </Link>
      </CardHeader>
      <ul className="divide-y">
        {expiring.slice(0, 5).map((c) => (
          <li
            key={`${c.resource}/${c.host}/${c.port}`}
            className="flex flex-wrap items-center gap-x-3 gap-y-1 px-4 py-3 text-sm"
          >
            <CertificateDays certificate={c} />
            <span className="font-mono text-xs break-all">
              {c.host}:{c.port}
            </span>
            <span className="text-muted">
              {c.resource}
              {c.chain_not_after && `, ends ${c.chain_not_after.slice(0, 10)}`}
            </span>
          </li>
        ))}
      </ul>
      {expiring.length > 5 && <CardBody className="text-sm text-muted">and {count(expiring.length - 5)} more</CardBody>}
    </Card>
  );
}

function Totals({ all }: { all: Schemas["InterfaceStatus"][] }) {
  const destinations = all.flatMap((i) => i.destinations);
  const waiting = destinations.reduce((n, d) => n + d.pending, 0);
  const oldest = Math.max(0, ...destinations.map((d) => d.health.oldest_waiting_s ?? 0));
  const quarantined = destinations.reduce((n, d) => n + d.quarantined, 0);
  const attention = all.filter((i) => problems(i).some((p) => describe(p.state).rank >= 2)).length;
  const stopped = all.filter((i) => i.state === "stopped").length;
  return (
    <dl className="grid grid-cols-2 gap-4 lg:grid-cols-4">
      <Tile label="Interfaces" value={count(all.length)} note={stopped ? `${stopped} stopped` : "all running"} />
      <Tile label="Need attention" value={count(attention)} tone={attention ? "bad" : "ok"} />
      <Tile label="Waiting" value={count(waiting)} note={waiting ? `oldest ${duration(oldest)}` : "nothing waiting"} />
      <Tile label="Quarantined" value={count(quarantined)} tone={quarantined ? "bad" : undefined} />
    </dl>
  );
}

function Tile({ label, value, note, tone }: { label: string; value: string; note?: string; tone?: "ok" | "bad" }) {
  return (
    <div className="rounded-lg border bg-panel p-4">
      <dt className="text-sm text-muted">{label}</dt>
      <dd
        className={cn(
          "mt-1 text-2xl font-semibold tabular-nums",
          tone === "bad" && "text-bad",
          tone === "ok" && "text-ok",
        )}
      >
        {value}
      </dd>
      {note && <dd className="mt-0.5 text-xs text-muted">{note}</dd>}
    </div>
  );
}

function Attention({ all }: { all: Schemas["InterfaceStatus"][] }) {
  const rows = all
    .flatMap((i) => problems(i).map((p) => ({ ...p, iface: i.name })))
    .sort((a, b) => describe(b.state).rank - describe(a.state).rank || a.iface.localeCompare(b.iface));
  return (
    <Card aria-label="Needs attention">
      <CardHeader>
        <CardTitle>Needs attention</CardTitle>
      </CardHeader>
      {rows.length === 0 ? (
        <CardBody className="flex items-center gap-2 text-sm text-ok">
          <CircleCheck aria-hidden className="size-5" />
          All {count(all.length)} interfaces are ok.
        </CardBody>
      ) : (
        <ul className="divide-y">
          {rows.map((p) => (
            <li
              key={`${p.iface}/${p.destination ?? ""}`}
              className="flex flex-wrap items-center gap-x-3 gap-y-1 px-4 py-3"
            >
              <HealthBadge state={p.state} />
              <Link to="/interfaces/$name" params={{ name: p.iface }} className="font-medium">
                {p.iface}
                {/* An interface with one destination calls it `default`: nothing to add. */}
                {p.destination && p.destination !== "default" && <span className="text-muted"> → {p.destination}</span>}
              </Link>
              <span className="text-sm text-muted">{p.summary}</span>
            </li>
          ))}
        </ul>
      )}
    </Card>
  );
}

function Resources({ resources }: { resources: Schemas["ResourceCalls"][] }) {
  if (resources.length === 0) return null;
  return (
    <Card>
      <CardHeader>
        <CardTitle>Databases and APIs</CardTitle>
        <span className="text-xs text-muted">calls from interface code since the engine started</span>
      </CardHeader>
      <Table>
        <thead>
          <tr>
            <Th>Name</Th>
            <Th>Kind</Th>
            <Th className="text-right">Calls</Th>
            <Th className="text-right">p50</Th>
            <Th className="text-right">p95</Th>
            <Th className="text-right">Failed</Th>
          </tr>
        </thead>
        <tbody>
          {resources.map((r) => (
            <tr key={`${r.kind}/${r.name}`}>
              <Td className="font-medium">{r.name}</Td>
              <Td>{r.kind}</Td>
              <Td className="text-right tabular-nums">{count(r.calls.count)}</Td>
              <Td className="text-right tabular-nums">{ms(r.calls.p50_ms)}</Td>
              <Td className="text-right tabular-nums">{ms(r.calls.p95_ms)}</Td>
              <Td className={cn("text-right tabular-nums", r.errors > 0 && "text-bad")}>{count(r.errors)}</Td>
            </tr>
          ))}
        </tbody>
      </Table>
    </Card>
  );
}
