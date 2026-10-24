import { useQuery } from "@tanstack/react-query";
import {
  CircleAlert,
  CircleCheck,
  Download,
  Lock,
  LockOpen,
  RefreshCw,
  Stethoscope,
  TriangleAlert,
} from "lucide-react";
import { useState, type FormEvent } from "react";
import { runDoctor, useCanOperate, useRefresh } from "@/api/actions";
import { ApiError, type Schemas } from "@/api/client";
import { certificates, interfaces, lastDoctorRun, ports } from "@/api/queries";
import { AllowList } from "@/components/allow-list";
import { CertificateDays } from "@/components/certificates";
import { Loaded, PageHeader } from "@/components/page";
import { Button } from "@/components/ui/button";
import { Card, CardBody, CardHeader, CardTitle } from "@/components/ui/card";
import { Table, Td, Th } from "@/components/ui/table";
import { ago, count, duration, timestamp } from "@/lib/format";
import { cn } from "@/lib/utils";

const OUTCOMES = {
  fail: { icon: CircleAlert, label: "Failed", className: "text-bad", rank: 0 },
  warn: { icon: TriangleAlert, label: "Warning", className: "text-warn", rank: 1 },
  ok: { icon: CircleCheck, label: "OK", className: "text-ok", rank: 2 },
} as const;

/**
 * The engine's connections: the certificates it connects to (from its own checks, every few hours),
 * and `ductus doctor` from the browser: every connection checked from the engine's server (DNS, TCP
 * with the source address a firewall request needs, TLS, then each system's login). Nothing is sent
 * to a destination.
 */
export function DoctorPage() {
  const canOperate = useCanOperate();
  const refresh = useRefresh();
  const all = useQuery(interfaces);
  const last = useQuery({ ...lastDoctorRun, enabled: canOperate });
  const [only, setOnly] = useState("");
  const [wait, setWait] = useState(5000);
  const [report, setReport] = useState<Schemas["Report"] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const run = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      setReport(await runDoctor(only ? [only] : [], wait));
      await refresh();
    } catch (err) {
      setError(err instanceof ApiError ? err.message : "The engine didn't answer.");
    } finally {
      setBusy(false);
    }
  };
  // The report shown: the one just run, else the last one the engine kept.
  const shown = report ?? last.data?.report;
  const lastRun = report ? null : last.data?.run;

  return (
    <>
      <PageHeader title="Connection checks">
        Every database, API, server and destination the interfaces use, checked from the engine's server: DNS, the
        connection (with the address a firewall rule needs), TLS, then each system's own login. Nothing is sent to a
        destination.
      </PageHeader>
      <Certificates />
      <Ports />
      <h2 className="mt-8 mb-3 text-lg font-semibold">Check every connection</h2>
      {!canOperate ? (
        <p className="text-sm text-muted">Running the checks needs an operator token.</p>
      ) : (
        <form onSubmit={run} className="mb-6 flex flex-wrap items-end gap-3">
          <label className="text-sm">
            <span className="block text-xs text-muted">Check</span>
            <select
              className="mt-1 h-9 rounded-md border bg-panel px-2 text-sm"
              value={only}
              onChange={(e) => setOnly(e.target.value)}
            >
              <option value="">Everything</option>
              {(all.data ?? []).map((i) => (
                <option key={i.name} value={i.name}>
                  What {i.name} uses
                </option>
              ))}
            </select>
          </label>
          <label className="text-sm">
            <span className="block text-xs text-muted">Wait per step</span>
            <select
              className="mt-1 h-9 rounded-md border bg-panel px-2 text-sm"
              value={wait}
              onChange={(e) => setWait(Number(e.target.value))}
            >
              {[2, 5, 10, 30].map((s) => (
                <option key={s} value={s * 1000}>
                  {s} s
                </option>
              ))}
            </select>
          </label>
          <Button type="submit" disabled={busy}>
            <Stethoscope aria-hidden />
            {busy ? "Checking…" : "Run the checks"}
          </Button>
        </form>
      )}
      {error && (
        <p role="alert" className="mb-4 rounded-md border border-bad/40 bg-bad/10 p-3 text-sm text-bad">
          {error}
        </p>
      )}
      {lastRun && (
        <p className="mb-2 text-sm text-muted">
          The last check: {timestamp(lastRun.at)}, {lastRun.mode === "full" ? "everything" : "certificates only"},{" "}
          {lastRun.started_by === "schedule" ? "by the engine on its schedule" : `asked for by ${lastRun.started_by}`}.
        </p>
      )}
      {shown && <Report report={shown} />}
    </>
  );
}

/** Every certificate the engine connects to, soonest expiry first, with the engine's own schedule. */
function Certificates() {
  const query = useQuery(certificates);
  const canOperate = useCanOperate();
  const refresh = useRefresh();
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<{ text: string; failed: boolean } | null>(null);
  const checkNow = async () => {
    setBusy(true);
    setNote(null);
    try {
      const r = await runDoctor([], 5000, "certificates");
      const read = r.checks.filter((c) => c.certificate?.fingerprint).length;
      setNote({ text: `Checked ${count(r.checks.length)}: ${count(read)} certificates read.`, failed: false });
    } catch (err) {
      setNote({ text: err instanceof ApiError ? err.message : "The engine didn't answer.", failed: true });
    } finally {
      setBusy(false);
      await refresh();
    }
  };
  return (
    <Card aria-label="Certificates">
      <CardHeader>
        <CardTitle>Certificates</CardTitle>
        {canOperate && (
          <Button size="sm" variant="outline" disabled={busy} onClick={() => void checkNow()}>
            <RefreshCw aria-hidden className={cn("size-4", busy && "animate-spin")} />
            {busy ? "Checking…" : "Check now"}
          </Button>
        )}
      </CardHeader>
      <Loaded query={query}>
        {(data) => {
          const remote = data.certificates.filter((c) => c.side !== "ours");
          const ours = data.certificates.filter((c) => c.side === "ours");
          return (
            <>
              <CardBody className="space-y-1 text-sm text-muted">
                <p>
                  {data.schedule_seconds == null
                    ? "The engine doesn't check by itself (doctor.schedule: off in ductus.yaml)."
                    : data.last_scheduled
                      ? `Last automatic check ${ago(data.last_scheduled.at)}; the engine checks every ${duration(data.schedule_seconds)} (${data.mode === "full" ? "everything" : "certificates only"}).`
                      : `The engine checks every ${duration(data.schedule_seconds)}; its first check runs a minute or two after it starts.`}{" "}
                  Days are whole days in UTC, to the earliest expiry in each chain.
                </p>
                {!data.alerting && (
                  <p>No alert rule names certificate: nothing is sent about them (alerts.rules in ductus.yaml).</p>
                )}
                {note && (
                  <p role={note.failed ? "alert" : "status"} className={note.failed ? "text-bad" : undefined}>
                    {note.text}
                  </p>
                )}
              </CardBody>
              {remote.length === 0 ? (
                <CardBody className="text-sm text-muted">Nothing the engine connects to uses TLS.</CardBody>
              ) : (
                <Table aria-label="Certificates">
                  <thead>
                    <tr>
                      <Th>Days left</Th>
                      <Th>Server</Th>
                      <Th>Certificate</Th>
                      <Th>Expires</Th>
                      <Th>Last check</Th>
                    </tr>
                  </thead>
                  <tbody>
                    {remote.map((c) => (
                      <tr key={`${c.resource}/${c.host}/${c.port}`}>
                        <Td>
                          <CertificateDays certificate={c} />
                        </Td>
                        <Td className="break-words">
                          <div className="font-mono text-xs">
                            {c.host}:{c.port}
                          </div>
                          <div className="text-xs text-muted">{c.resource}</div>
                        </Td>
                        <Td className="text-xs break-words">
                          {c.subject ? (
                            <>
                              <div>{c.subject}</div>
                              <div className="text-muted">
                                {c.issuer === c.subject ? "self-signed" : `issued by ${c.issuer ?? "?"}`}
                                {c.trusted === false && ", not trusted by this server"}
                              </div>
                              {c.chain_earliest && (
                                <div>The earliest expiry in its chain is {c.chain_earliest}&apos;s.</div>
                              )}
                            </>
                          ) : (
                            <span className="text-muted">none read</span>
                          )}
                        </Td>
                        <Td className="text-xs tabular-nums">{c.chain_not_after?.slice(0, 10) ?? "–"}</Td>
                        <Td className="max-w-xs text-xs break-words">
                          <div>{ago(c.checked_at)}</div>
                          {c.error && <div className="text-muted">{c.error}</div>}
                          {c.read_at && c.checked_at && c.read_at < c.checked_at && (
                            <div className="text-muted">dates from {timestamp(c.read_at)}</div>
                          )}
                        </Td>
                      </tr>
                    ))}
                  </tbody>
                </Table>
              )}
              {ours.length > 0 && <Ours certificates={ours} />}
            </>
          );
        }}
      </Loaded>
    </Card>
  );
}

/**
 * The certificates the engine presents (to senders on its TLS listeners, or as its client certificate), as it
 * loaded them: no check needed, and a renewal shows within a minute.
 */
function Ours({ certificates }: { certificates: Schemas["CertificateInfo"][] }) {
  return (
    <section aria-label="Ours" className="border-t">
      <h3 className="px-4 pt-4 text-sm font-semibold">Ours</h3>
      <p className="px-4 pb-2 text-sm text-muted">
        What the engine presents, as loaded from resources.yaml. Renewed files are picked up within a minute (or at
        Check now), without a restart.
      </p>
      <Table aria-label="Our certificates">
        <thead>
          <tr>
            <Th>Days left</Th>
            <Th>Ours</Th>
            <Th>Certificate</Th>
            <Th>Expires</Th>
            <Th>Renewal</Th>
          </tr>
        </thead>
        <tbody>
          {certificates.map((c) => (
            <tr key={c.resource}>
              <Td>
                <CertificateDays certificate={c} />
              </Td>
              <Td className="break-words">
                <div className="font-medium">{c.resource}</div>
                <ul className="text-xs text-muted">
                  {c.presented.map((p) => (
                    <li key={p}>{p}</li>
                  ))}
                </ul>
              </Td>
              <Td className="text-xs break-words">
                <div>{c.subject}</div>
                <div className="text-muted">
                  {c.issuer === c.subject ? "self-signed" : `issued by ${c.issuer ?? "?"}`}
                </div>
                {c.chain_earliest && <div>The earliest expiry in its chain is {c.chain_earliest}&apos;s.</div>}
              </Td>
              <Td className="text-xs tabular-nums">{c.chain_not_after?.slice(0, 10) ?? "–"}</Td>
              <Td className="max-w-xs text-xs break-words">
                {c.error ? (
                  <span className="inline-flex gap-1 text-bad">
                    <CircleAlert aria-hidden className="mt-px size-3.5 shrink-0" />
                    <span>The new files can&apos;t be used: {c.error}. Still presenting this one.</span>
                  </span>
                ) : (
                  <span className="text-muted">In use as loaded</span>
                )}
              </Td>
            </tr>
          ))}
        </tbody>
      </Table>
    </section>
  );
}

/** What the engine listens on and connects to, with TLS in and out: the same report as `ductus ports`. */
function Ports() {
  const query = useQuery(ports);
  return (
    <Card aria-label="Ports" className="mt-6">
      <CardHeader>
        <CardTitle>Ports</CardTitle>
      </CardHeader>
      <Loaded query={query}>
        {(data) => (
          <>
            <h3 className="px-4 pt-1 text-sm font-semibold">Listening</h3>
            <Table aria-label="Listening">
              <thead>
                <tr>
                  <Th>Port</Th>
                  <Th>Protocol</Th>
                  <Th>Interface</Th>
                  <Th>TLS</Th>
                  <Th>Allowed</Th>
                </tr>
              </thead>
              <tbody>
                {data.listening.map((l) => (
                  <tr key={`${l.port}/${l.interface}`}>
                    <Td className="font-mono text-xs">
                      {l.host}:{l.port}
                    </Td>
                    <Td className="text-xs whitespace-nowrap">
                      <Secure on={l.tls !== "none"} />
                      {l.protocol}
                    </Td>
                    <Td className="text-xs break-words">
                      {l.interface}
                      {l.route && <div className="text-muted">{l.route}</div>}
                    </Td>
                    <Td className="text-xs break-words">{l.tls}</Td>
                    <Td className="max-w-xs break-words">
                      {l.allow_detail ? (
                        <AllowList allow={l.allow_detail} />
                      ) : (
                        <span className="text-xs text-muted">anyone</span>
                      )}
                    </Td>
                  </tr>
                ))}
              </tbody>
            </Table>
            <h3 className="border-t px-4 pt-4 text-sm font-semibold">Connects to</h3>
            <Table aria-label="Connects to">
              <thead>
                <tr>
                  <Th>Server</Th>
                  <Th>Protocol</Th>
                  <Th>Used by</Th>
                  <Th>TLS</Th>
                </tr>
              </thead>
              <tbody>
                {data.connects_to.map((o, n) => (
                  <tr key={n}>
                    <Td className="font-mono text-xs break-all">
                      {o.host}:{o.port}
                    </Td>
                    <Td className="text-xs whitespace-nowrap">
                      {o.tls && <Secure on={!o.tls.startsWith("none") && !o.tls.includes("not verified")} />}
                      {o.protocol}
                    </Td>
                    <Td className="text-xs break-words">{o.used_by}</Td>
                    <Td className="text-xs break-words">{o.tls || <span className="text-muted">–</span>}</Td>
                  </tr>
                ))}
              </tbody>
            </Table>
          </>
        )}
      </Loaded>
    </Card>
  );
}

/** A lock, closed or open, beside the protocol (whose words say the same: never the icon alone). */
function Secure({ on }: { on: boolean }) {
  const Icon = on ? Lock : LockOpen;
  return <Icon aria-hidden className={cn("mr-1 inline size-3.5 align-[-2px]", on ? "text-ok" : "text-muted")} />;
}

function Report({ report }: { report: Schemas["Report"] }) {
  const checks = [...report.checks].sort((a, b) => OUTCOMES[a.outcome].rank - OUTCOMES[b.outcome].rank);
  const tally = (o: Schemas["Outcome"]) => report.checks.filter((c) => c.outcome === o).length;
  const save = () => {
    const url = URL.createObjectURL(new Blob([JSON.stringify(report, null, 2)], { type: "application/json" }));
    const a = document.createElement("a");
    a.href = url;
    a.download = `doctor-${report.host}-${report.generated_at.slice(0, 10)}.json`;
    a.click();
    URL.revokeObjectURL(url);
  };
  return (
    <section aria-label="Report" className="space-y-4">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <p role="status" className="text-sm">
          <strong>
            {count(tally("ok"))} ok, {count(tally("warn"))} {tally("warn") === 1 ? "warning" : "warnings"},{" "}
            {count(tally("fail"))} failed
          </strong>{" "}
          <span className="text-muted">
            · from {report.host} ({report.environment}), {report.generated_at}, {report.seconds.toFixed(1)} s
          </span>
        </p>
        <Button variant="outline" onClick={save}>
          <Download aria-hidden />
          Save the report
        </Button>
      </div>
      <Card>
        <Table aria-label="Checks">
          <thead>
            <tr>
              <Th>Result</Th>
              <Th>What</Th>
              <Th>Steps</Th>
              <Th>What to do</Th>
            </tr>
          </thead>
          <tbody>
            {checks.map((c, n) => {
              const o = OUTCOMES[c.outcome];
              return (
                <tr key={n}>
                  <Td>
                    <span className={cn("inline-flex items-center gap-1 text-sm font-medium", o.className)}>
                      <o.icon aria-hidden className="size-4" />
                      {o.label}
                    </span>
                  </Td>
                  <Td>
                    <div className="font-medium">{c.what}</div>
                    <div className="font-mono text-xs">{c.endpoint}</div>
                    {c.used_by.length > 0 && <div className="text-xs text-muted">used by {c.used_by.join(", ")}</div>}
                  </Td>
                  <Td>
                    <ul className="space-y-0.5 text-xs">
                      {c.steps.map((s, k) => {
                        const so = OUTCOMES[s.outcome];
                        return (
                          <li key={k} className="flex gap-1.5">
                            <so.icon aria-label={so.label} className={cn("mt-px size-3.5 shrink-0", so.className)} />
                            <span>
                              <strong>{s.name}</strong> {s.detail}
                            </span>
                          </li>
                        );
                      })}
                    </ul>
                  </Td>
                  <Td className="max-w-md text-xs">
                    {c.advice.map((a, k) => (
                      <p key={k}>{a}</p>
                    ))}
                  </Td>
                </tr>
              );
            })}
          </tbody>
        </Table>
      </Card>
    </section>
  );
}
