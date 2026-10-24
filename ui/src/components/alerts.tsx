import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { BellOff, CircleAlert, CircleCheck, TriangleAlert } from "lucide-react";
import { useState } from "react";
import { acknowledgeAlert, silenceAlert, unsilenceAlert, useCanOperate, useRefresh } from "@/api/actions";
import { ApiError, type Schemas } from "@/api/client";
import { alerts } from "@/api/queries";
import { ActionDialog } from "@/components/action-dialog";
import { Button } from "@/components/ui/button";
import { ago, timestamp } from "@/lib/format";
import { cn } from "@/lib/utils";

export type Alert = Schemas["AlertInfo"];

/** Open alerts of this engine (and how many are critical), from the shared query. */
export function useOpenAlerts() {
  const { data } = useQuery(alerts);
  const open = (data?.alerts ?? []).filter((a) => a.state !== "ok");
  return {
    configured: data?.configured ?? false,
    open,
    critical: open.filter((a) => a.severity === "critical").length,
  };
}

/**
 * A certificate's alert (`cert:<resource>:<host>:<port>`): `interface` is its resource, `destination` the server.
 * Ours (one the engine presents) are at port 0, which no server can be (`destination` is the engine's server).
 */
export function isCertificate(a: { key: string }) {
  return a.key.startsWith("cert:");
}

/** `adt-relay → lab`, `adt-relay` (its own alert, or its only destination), `certificate of host:443 (empi)` or `our certificate ductus-mllp (on engine-1)`. */
export function alertName(a: { key: string; interface: string; destination?: string | null }) {
  if (isCertificate(a) && a.key.endsWith(":0")) return `our certificate ${a.interface} (on ${a.destination ?? "?"})`;
  if (isCertificate(a)) return `certificate of ${a.destination ?? "?"} (${a.interface})`;
  return a.destination && a.destination !== "default" ? `${a.interface} → ${a.destination}` : a.interface;
}

/** The alert's name, linking to where it's about: the interface, or the Connections page for a certificate. */
export function AlertLink({ alert, className }: { alert: Alert; className?: string }) {
  return isCertificate(alert) ? (
    <Link to="/connections" className={className}>
      {alertName(alert)}
    </Link>
  ) : (
    <Link to="/interfaces/$name" params={{ name: alert.interface }} className={className}>
      {alertName(alert)}
    </Link>
  );
}

const SEVERITY = {
  critical: { icon: CircleAlert, label: "Critical", className: "text-bad bg-bad/10 border-bad/40" },
  warning: { icon: TriangleAlert, label: "Warning", className: "text-warn bg-warn/10 border-warn/40" },
  ok: { icon: CircleCheck, label: "Fine", className: "text-ok bg-ok/10 border-ok/30" },
};

/** A severity as an icon and a word (never color alone). */
export function SeverityBadge({ severity }: { severity?: string | null }) {
  const s = SEVERITY[(severity as keyof typeof SEVERITY) ?? "ok"] ?? SEVERITY.ok;
  const Icon = s.icon;
  return (
    <span
      className={cn(
        "inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-xs font-medium whitespace-nowrap",
        s.className,
      )}
    >
      <Icon aria-hidden className="size-3.5" />
      {s.label}
    </span>
  );
}

/** Who acknowledged it, its silence, whether it is settling: one line of small print. */
export function AlertNotes({ alert }: { alert: Alert }) {
  const notes: string[] = [];
  if (alert.acknowledged_by)
    notes.push(`acknowledged by ${alert.acknowledged_by} ${ago(alert.acknowledged_at)}`.trim());
  if (alert.silenced_until)
    notes.push(
      `silenced by ${alert.silenced_by ?? "?"} until ${timestamp(alert.silenced_until)}` +
        (alert.silence_reason ? `: ${alert.silence_reason}` : ""),
    );
  if (alert.recovering_since) notes.push(`fine again since ${timestamp(alert.recovering_since)}, settling`);
  if (alert.muted) notes.push("muted in interface.yaml: nothing is sent");
  if (notes.length === 0) return null;
  return <p className="text-xs text-muted">{notes.join(" · ")}</p>;
}

const DURATIONS: [string, string][] = [
  ["1h", "1 hour"],
  ["4h", "4 hours"],
  ["8h", "8 hours"],
  ["1d", "1 day"],
  ["3d", "3 days"],
  ["7d", "7 days"],
];

/** Acknowledge, silence and end a silence (operators; the engine checks too). */
export function AlertActions({ alert }: { alert: Alert }) {
  const canOperate = useCanOperate();
  const refresh = useRefresh();
  const [silencing, setSilencing] = useState(false);
  const [duration, setDuration] = useState("4h");
  const [error, setError] = useState<string | null>(null);
  if (!canOperate) return null;
  const name = alertName(alert);
  const act = async (f: () => Promise<unknown>) => {
    setError(null);
    try {
      await f();
    } catch (e) {
      setError(e instanceof ApiError ? e.message : "The engine didn't answer.");
    }
    await refresh();
  };
  return (
    <div className="flex flex-wrap items-center gap-2">
      {alert.state !== "ok" && !alert.acknowledged_by && (
        <Button size="sm" variant="outline" onClick={() => void act(() => acknowledgeAlert(alert.id))}>
          Acknowledge<span className="sr-only"> the alert of {name}</span>
        </Button>
      )}
      {alert.silenced_until ? (
        <Button size="sm" variant="outline" onClick={() => void act(() => unsilenceAlert(alert.id))}>
          End silence<span className="sr-only"> of {name}</span>
        </Button>
      ) : (
        <Button size="sm" variant="outline" onClick={() => setSilencing(true)}>
          <BellOff aria-hidden className="size-4" />
          Silence…<span className="sr-only"> {name}</span>
        </Button>
      )}
      {error && (
        <span role="alert" className="text-xs text-bad">
          {error}
        </span>
      )}
      <ActionDialog
        open={silencing}
        onOpenChange={setSilencing}
        title={`Silence ${name}`}
        description="Nothing is sent about it until the time is up: not when it fires, gets worse, reminds or recovers. It still shows here. Messages still wait: to stop sending, pause the destination."
        confirm="Silence"
        onConfirm={async (why) => {
          await silenceAlert(alert.id, duration, why);
          await refresh();
        }}
      >
        <label className="block text-sm">
          <span className="text-muted">For</span>
          <select
            className="mt-1 block h-9 w-full rounded-md border bg-panel px-2 text-sm"
            value={duration}
            onChange={(e) => setDuration(e.target.value)}
          >
            {DURATIONS.map(([value, label]) => (
              <option key={value} value={value}>
                {label}
              </option>
            ))}
          </select>
        </label>
      </ActionDialog>
    </div>
  );
}

/** One alert as a list item: severity, name, state and since when, the health words, notes, actions. */
export function AlertItem({ alert, link = true }: { alert: Alert; link?: boolean }) {
  const name = alertName(alert);
  return (
    <li className="flex flex-wrap items-start justify-between gap-3 px-4 py-3">
      <div className="min-w-0 space-y-1">
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
          <SeverityBadge severity={alert.state === "ok" ? null : alert.severity} />
          {link ? <AlertLink alert={alert} className="font-medium" /> : <span className="font-medium">{name}</span>}
          <span className="text-sm">
            {alert.state === "ok" ? "fine" : alert.state}
            {alert.since && <span className="text-muted"> since {timestamp(alert.since)}</span>}
          </span>
        </div>
        {alert.state !== "ok" && alert.summary && <p className="text-sm break-words text-muted">{alert.summary}</p>}
        <AlertNotes alert={alert} />
      </div>
      <AlertActions alert={alert} />
    </li>
  );
}
