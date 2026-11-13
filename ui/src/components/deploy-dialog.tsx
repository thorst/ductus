import { CircleAlert, CircleCheck } from "lucide-react";
import { useState, type FormEvent } from "react";
import { checkRelease, deployRelease, useRefresh } from "@/api/actions";
import { ApiError, type Schemas } from "@/api/client";
import { GateReport } from "@/components/gate-report";
import { Button } from "@/components/ui/button";
import { Dialog } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { changesSummary, reuseCaveat, rulesText } from "@/lib/releases";

/**
 * Deploying a release here, or rolling back to one: this environment's rules and what it changes
 * first; **Check first** runs the gate and shows its report, then **Deploy** reuses that check
 * (the pre-run) instead of running it again. A refusal shows the engine's report: its rules, or
 * whose work it would undo, and only then the choice to deploy over that work (forced, recorded).
 */
export function DeployDialog({
  open,
  onOpenChange,
  release,
  short,
  rollback = false,
  environment,
  mode,
  here,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** The release's id (or its first 12 characters). */
  release: string;
  short: string;
  rollback?: boolean;
  environment: string;
  mode: Schemas["ReleaseMode"];
  here?: Schemas["Here"] | null;
}) {
  const refresh = useRefresh();
  const [why, setWhy] = useState("");
  const [check, setCheck] = useState<Schemas["CheckInfo"] | null>(null);
  const [report, setReport] = useState<Schemas["DeployReport"] | null>(null);
  const [force, setForce] = useState(false);
  const [busy, setBusy] = useState<"check" | "deploy" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const reasonRequired = environment !== "dev";
  const verb = rollback ? "Roll back" : "Deploy";

  const close = (o: boolean) => {
    if (!o) {
      setCheck(null);
      setReport(null);
      setForce(false);
      setError(null);
      setWhy("");
    }
    onOpenChange(o);
  };
  const runCheck = async () => {
    setBusy("check");
    setError(null);
    try {
      setCheck(await checkRelease(release));
    } catch (err) {
      setError(err instanceof ApiError ? err.message : "The engine didn't answer.");
    } finally {
      setBusy(null);
    }
  };
  const deploy = async (e: FormEvent) => {
    e.preventDefault();
    setBusy("deploy");
    setError(null);
    try {
      const answer = await deployRelease({
        release,
        reason: why.trim() || undefined,
        rollback,
        force: force || undefined,
      });
      setReport(answer);
      if (answer.ok) await refresh();
    } catch (err) {
      setError(err instanceof ApiError ? err.message : "The engine didn't answer.");
    } finally {
      setBusy(null);
    }
  };

  const replaces = Object.entries(report?.replaces ?? {});
  const canForce = !rollback && report !== null && !report.ok && replaces.length > 0 && report.rules.length === 0;
  const caveat = reuseCaveat(mode);
  return (
    <Dialog
      open={open}
      onOpenChange={close}
      wide
      title={rollback ? `Roll ${environment} back to release ${short}` : `Deploy release ${short} to ${environment}`}
      description={
        rollback
          ? "Goes back to a release that ran here before. This environment's rules don't hold it; the gate runs."
          : "This environment's rules first, then whose work it would undo, then the gate; then the engine hands over to one running it. Receiving never stops."
      }
    >
      {report?.ok ? (
        <Done report={report} onClose={() => close(false)} />
      ) : (
        <form onSubmit={deploy} className="space-y-4">
          {here && !rollback && <p className="text-sm">{changesSummary(here)}</p>}
          {!rollback && (
            <ul className="list-disc space-y-0.5 pl-5 text-sm text-muted">
              {rulesText(mode, environment).map((r) => (
                <li key={r}>{r}</li>
              ))}
            </ul>
          )}
          {!rollback && (
            <div className="space-y-2 rounded-md border p-3">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <span className="text-sm">
                  {check ? "The check:" : "Check first: the gate against this environment, nothing changed."}
                </span>
                <Button type="button" variant="outline" size="sm" disabled={busy !== null} onClick={runCheck}>
                  {busy === "check" ? "Checking…" : check ? "Check again" : "Check first"}
                </Button>
              </div>
              {caveat && <p className="text-xs text-warn">{caveat}</p>}
              {check && <GateReport check={check} />}
            </div>
          )}
          {report && !report.ok && <Refused report={report} />}
          {canForce && (
            <label className="flex items-start gap-2 text-sm">
              <input type="checkbox" className="mt-1" checked={force} onChange={(e) => setForce(e.target.checked)} />
              <span>
                Deploy over their work: it&rsquo;s undone here, and who forced it and why go into the audit log and the
                history.
              </span>
            </label>
          )}
          <label className="block text-sm">
            <span className="text-muted">
              {reasonRequired ? "Reason (required, kept with the deployment)" : "Reason (optional in dev)"}
            </span>
            <Input className="mt-1" value={why} required={reasonRequired} onChange={(e) => setWhy(e.target.value)} />
          </label>
          {error && (
            <p role="alert" className="rounded-md border border-bad/40 bg-bad/10 p-3 text-sm text-bad">
              {error}
            </p>
          )}
          <div className="flex justify-end gap-2">
            <Button type="button" variant="outline" onClick={() => close(false)}>
              Cancel
            </Button>
            <Button
              type="submit"
              disabled={busy !== null || (reasonRequired && !why.trim()) || check?.ok === false || (canForce && !force)}
            >
              {busy === "deploy" ? "Working… (the gate, then the handover)" : verb}
            </Button>
          </div>
        </form>
      )}
    </Dialog>
  );
}

/** What a refused or failed deploy said: its outcome, the rules, whose work, the gate's report. */
function Refused({ report }: { report: Schemas["DeployReport"] }) {
  const replaces = Object.entries(report.replaces);
  return (
    <div role="alert" className="space-y-2 rounded-md border border-bad/40 bg-bad/10 p-3 text-sm">
      <p className="flex gap-1.5 font-medium text-bad">
        <CircleAlert aria-hidden className="mt-0.5 size-4 shrink-0" />
        {report.outcome}
      </p>
      {report.rules.length > 0 && (
        <ul className="list-disc space-y-0.5 pl-5">
          {report.rules.map((r) => (
            <li key={r}>{r}</li>
          ))}
        </ul>
      )}
      {replaces.length > 0 && (
        <div>
          <p>It would undo someone else&rsquo;s work:</p>
          <ul className="list-disc space-y-0.5 pl-5">
            {replaces.map(([what, why]) => (
              <li key={what}>
                <strong>{what}</strong>: {why}
              </li>
            ))}
          </ul>
        </div>
      )}
      {report.gate && !report.gate.check.ok && <GateReport check={report.gate.check} />}
      {report.steps.length > 0 && (
        <pre className="max-h-40 overflow-auto text-xs whitespace-pre-wrap">{report.steps.join("\n")}</pre>
      )}
    </div>
  );
}

/** A deploy that went through: what happened, what changed. */
function Done({ report, onClose }: { report: Schemas["DeployReport"]; onClose: () => void }) {
  return (
    <div className="space-y-3 text-sm">
      <p role="status" className="flex gap-1.5 font-medium text-ok">
        <CircleCheck aria-hidden className="mt-0.5 size-4 shrink-0" />
        {report.outcome}
      </p>
      {report.gate && (
        <p className="text-muted">
          {report.gate.reused
            ? `The gate: the check from ${report.gate.check.by} was reused.`
            : `The gate passed in ${report.gate.check.seconds.toFixed(1)} s.`}
        </p>
      )}
      {report.changes.length > 0 && (
        <ul className="space-y-0.5">
          {report.changes.map((c) => (
            <li key={c.interface}>
              <span className="font-medium">{c.interface}</span>{" "}
              <span className="font-mono text-xs">{c.from ? `${c.from} → ${c.to}` : `new here ${c.to}`}</span>
              {c.warning && <div className="text-xs text-warn">{c.warning}</div>}
            </li>
          ))}
        </ul>
      )}
      <div className="flex justify-end">
        <Button onClick={onClose}>Done</Button>
      </div>
    </div>
  );
}
