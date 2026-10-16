import { useQuery } from "@tanstack/react-query";
import { CircleCheck, Send } from "lucide-react";
import { useState } from "react";
import { testChannel, useCanOperate, useRefresh } from "@/api/actions";
import { ApiError, type Schemas } from "@/api/client";
import { alertChannels, alerts } from "@/api/queries";
import { AlertItem, AlertLink, SeverityBadge } from "@/components/alerts";
import { Loaded, PageHeader } from "@/components/page";
import { Button } from "@/components/ui/button";
import { Card, CardBody, CardHeader, CardTitle } from "@/components/ui/card";
import { Table, Td, Th } from "@/components/ui/table";
import { count, timestamp } from "@/lib/format";

/** An event in plain words. */
const KINDS: Record<string, string> = {
  fired: "Fired",
  escalated: "Got worse",
  reminded: "Reminded",
  recovered: "Recovered",
  acknowledged: "Acknowledged",
  silenced: "Silenced",
  unsilenced: "Silence ended",
};

/** What is wrong, what happened, and where notifications go. */
export function AlertsPage() {
  const query = useQuery(alerts);
  const canOperate = useCanOperate();
  return (
    <>
      <PageHeader title="Alerts">
        Interfaces and destinations whose health turned bad, as the rules in <code>ductus.yaml</code> say. Acknowledge
        one to stop its reminders; silence one for planned work.
      </PageHeader>
      <Loaded query={query}>
        {(overview) =>
          !overview.configured ? (
            <NotConfigured />
          ) : (
            <div className="space-y-6">
              <Open all={overview.alerts} />
              <Silenced all={overview.alerts} />
              <History events={overview.recent} all={overview.alerts} />
              {canOperate && <Channels />}
            </div>
          )
        }
      </Loaded>
    </>
  );
}

function NotConfigured() {
  return (
    <div className="max-w-prose space-y-3 text-sm">
      <p>
        Alerts aren't set up: nothing is judged or sent. Add <code>alerts:</code> to <code>ductus.yaml</code> (channels
        and rules), for example:
      </p>
      <pre className="overflow-x-auto rounded-md border bg-panel p-3 text-xs">{`alerts:
  smtp: { host: smtp.hospital.org, from: ductus@hospital.org,
          username: ductus, password: "\${secret:SMTP_PASSWORD}" }
  channels:
    oncall: { email: [interfaces-oncall@hospital.org] }
  rules:
    - { when: [error, stalled, stopped], severity: critical, to: [oncall, owner] }
    - { when: [behind, silent, missed], severity: warning, to: [owner] }`}</pre>
    </div>
  );
}

function Open({ all }: { all: Schemas["AlertInfo"][] }) {
  const open = all.filter((a) => a.state !== "ok");
  return (
    <Card aria-label="Open alerts">
      <CardHeader>
        <CardTitle>Open</CardTitle>
        <span className="text-xs text-muted">{count(all.length)} interfaces and destinations watched</span>
      </CardHeader>
      {open.length === 0 ? (
        <CardBody className="flex items-center gap-2 text-sm text-ok">
          <CircleCheck aria-hidden className="size-5" />
          Nothing is open.
        </CardBody>
      ) : (
        <ul className="divide-y">
          {open.map((a) => (
            <AlertItem key={a.id} alert={a} />
          ))}
        </ul>
      )}
    </Card>
  );
}

/** Fine but silenced (planned work): worth seeing, so a forgotten silence ends. */
function Silenced({ all }: { all: Schemas["AlertInfo"][] }) {
  const silenced = all.filter((a) => a.state === "ok" && a.silenced_until);
  if (silenced.length === 0) return null;
  return (
    <Card aria-label="Silenced">
      <CardHeader>
        <CardTitle>Silenced</CardTitle>
      </CardHeader>
      <ul className="divide-y">
        {silenced.map((a) => (
          <AlertItem key={a.id} alert={a} />
        ))}
      </ul>
    </Card>
  );
}

function History({ events, all }: { events: Schemas["AlertEvent"][]; all: Schemas["AlertInfo"][] }) {
  const byId = new Map(all.map((a) => [a.id, a]));
  return (
    <Card aria-label="Alert history">
      <CardHeader>
        <CardTitle>History</CardTitle>
        <span className="text-xs text-muted">the latest 50</span>
      </CardHeader>
      {events.length === 0 ? (
        <CardBody className="text-sm text-muted">Nothing has happened yet.</CardBody>
      ) : (
        <Table aria-label="Alert history">
          <thead>
            <tr>
              <Th>When</Th>
              <Th>What</Th>
              <Th>About</Th>
              <Th>State</Th>
              <Th>Notifications</Th>
              <Th>By</Th>
            </tr>
          </thead>
          <tbody>
            {events.map((e) => {
              const a = byId.get(e.alert);
              const notified = [
                e.sent && `${e.sent} sent`,
                e.waiting && `${e.waiting} to send`,
                e.failed && `${e.failed} failed`,
              ].filter(Boolean);
              return (
                <tr key={e.id} className="hover:bg-hover">
                  <Td className="tabular-nums">{timestamp(e.at)}</Td>
                  <Td>{KINDS[e.kind] ?? e.kind}</Td>
                  <Td className="break-words">{a ? <AlertLink alert={a} /> : e.key}</Td>
                  <Td>
                    <span className="flex flex-wrap items-center gap-2">
                      {e.severity && <SeverityBadge severity={e.severity} />}
                      {e.state}
                    </span>
                  </Td>
                  <Td className={e.failed ? "text-bad" : undefined}>{notified.join(", ") || "–"}</Td>
                  <Td>{e.by ?? ""}</Td>
                </tr>
              );
            })}
          </tbody>
        </Table>
      )}
    </Card>
  );
}

function Channels() {
  const query = useQuery(alertChannels);
  return (
    <Card aria-label="Channels">
      <CardHeader>
        <CardTitle>Channels</CardTitle>
        <span className="text-xs text-muted">the last 7 days; secrets are never shown</span>
      </CardHeader>
      <Loaded query={query}>
        {(c) => (
          <>
            <CardBody className="space-y-1 text-sm">
              <p>
                <span className="text-muted">Mail server: </span>
                {c.smtp ? `${c.smtp.host}:${c.smtp.port} (${c.smtp.tls}), from ${c.smtp.from}` : "none"}
              </p>
              <p>
                <span className="text-muted">Reminders: </span>
                {c.repeat ? `every ${c.repeat} until acknowledged` : "none"}
                {c.quiet && (
                  <>
                    <span className="text-muted"> · Quiet hours: </span>
                    {c.quiet}
                  </>
                )}
              </p>
              {c.heartbeat && (
                <p>
                  <span className="text-muted">Heartbeat: </span>
                  {c.heartbeat}
                </p>
              )}
            </CardBody>
            <Table aria-label="Channels">
              <thead>
                <tr>
                  <Th>Channel</Th>
                  <Th>Sends to</Th>
                  <Th className="text-right">Sent</Th>
                  <Th className="text-right">To send</Th>
                  <Th className="text-right">Failed</Th>
                  <Th>Last error</Th>
                  <Th>
                    <span className="sr-only">Test</span>
                  </Th>
                </tr>
              </thead>
              <tbody>
                {c.channels.map((ch) => (
                  <tr key={ch.name}>
                    <Td className="font-medium">{ch.name}</Td>
                    <Td className="text-xs break-words">
                      {ch.email.join(", ")}
                      {ch.webhook && (
                        <span className="block">
                          {ch.webhook} ({ch.webhook_format})
                        </span>
                      )}
                    </Td>
                    <Td className="text-right tabular-nums">{count(ch.sent)}</Td>
                    <Td className="text-right tabular-nums">{count(ch.waiting)}</Td>
                    <Td className={ch.failed ? "text-right text-bad tabular-nums" : "text-right tabular-nums"}>
                      {count(ch.failed)}
                    </Td>
                    <Td className="max-w-xs text-xs break-words">{ch.last_error ?? ""}</Td>
                    <Td>
                      <TestButton name={ch.name} />
                    </Td>
                  </tr>
                ))}
              </tbody>
            </Table>
            {c.owners.length > 0 && (
              <CardBody className="border-t text-sm">
                <h3 className="mb-2 font-medium">Owners</h3>
                <ul className="space-y-1">
                  {c.owners.map((o) => (
                    <li key={o.owner}>
                      {o.owner}:{" "}
                      {o.addresses.length ? (
                        o.addresses.join(", ")
                      ) : (
                        <span className="text-bad">
                          reaches nobody (no user with that name and an email, not in <code>owners:</code>)
                        </span>
                      )}
                    </li>
                  ))}
                </ul>
                {c.owner_notifications && (
                  <p className="mt-2 text-xs text-muted">
                    To owners: {count(c.owner_notifications.sent)} sent, {count(c.owner_notifications.waiting)} to send,{" "}
                    {count(c.owner_notifications.failed)} failed
                    {c.owner_notifications.last_error && ` (last error: ${c.owner_notifications.last_error})`}
                  </p>
                )}
              </CardBody>
            )}
          </>
        )}
      </Loaded>
    </Card>
  );
}

function TestButton({ name }: { name: string }) {
  const refresh = useRefresh();
  const [result, setResult] = useState<{ ok: boolean; text: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const send = async () => {
    setBusy(true);
    setResult(null);
    try {
      await testChannel(name);
      setResult({ ok: true, text: "Sent" });
    } catch (e) {
      setResult({ ok: false, text: e instanceof ApiError ? e.message : "The engine didn't answer." });
    } finally {
      setBusy(false);
      await refresh();
    }
  };
  return (
    <div className="space-y-1">
      <Button size="sm" variant="outline" disabled={busy} onClick={() => void send()}>
        <Send aria-hidden className="size-4" />
        {busy ? "Sending…" : "Send test"}
        <span className="sr-only"> to {name}</span>
      </Button>
      {result && (
        <p role="status" className={result.ok ? "text-xs text-ok" : "max-w-xs text-xs break-words text-bad"}>
          {result.text}
        </p>
      )}
    </div>
  );
}
