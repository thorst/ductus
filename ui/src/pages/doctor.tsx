import { useQuery } from "@tanstack/react-query";
import { CircleAlert, CircleCheck, Download, Stethoscope, TriangleAlert } from "lucide-react";
import { useState, type FormEvent } from "react";
import { runDoctor, useCanOperate } from "@/api/actions";
import { ApiError, type Schemas } from "@/api/client";
import { interfaces } from "@/api/queries";
import { PageHeader } from "@/components/page";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Table, Td, Th } from "@/components/ui/table";
import { count } from "@/lib/format";
import { cn } from "@/lib/utils";

const OUTCOMES = {
  fail: { icon: CircleAlert, label: "Failed", className: "text-bad", rank: 0 },
  warn: { icon: TriangleAlert, label: "Warning", className: "text-warn", rank: 1 },
  ok: { icon: CircleCheck, label: "OK", className: "text-ok", rank: 2 },
} as const;

/**
 * `ductus doctor` from the browser: every connection the engine needs, checked from its own
 * server (DNS, TCP with the source address a firewall request needs, TLS, then each system's
 * login). Nothing is sent to a destination.
 */
export function DoctorPage() {
  const canOperate = useCanOperate();
  const all = useQuery(interfaces);
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
    } catch (err) {
      setError(err instanceof ApiError ? err.message : "The engine didn't answer.");
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      <PageHeader title="Connection checks">
        Every database, API, server and destination the interfaces use, checked from the engine's server: DNS, the
        connection (with the address a firewall rule needs), TLS, then each system's own login. Nothing is sent to a
        destination.
      </PageHeader>
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
      {report && <Report report={report} />}
    </>
  );
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
