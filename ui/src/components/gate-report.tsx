import { CircleAlert, CircleCheck, Info, TriangleAlert } from "lucide-react";
import type { Schemas } from "@/api/client";
import { gateSections, sectionTitle, type Level } from "@/lib/releases";
import { count, timestamp } from "@/lib/format";
import { cn } from "@/lib/utils";

const LEVEL: Record<Level, { icon: typeof CircleCheck; label: string; className: string }> = {
  ok: { icon: CircleCheck, label: "OK", className: "text-ok" },
  note: { icon: Info, label: "Note", className: "text-muted" },
  warning: { icon: TriangleAlert, label: "Warning", className: "text-warn" },
  problem: { icon: CircleAlert, label: "Problem", className: "text-bad" },
};

/** One check's verdict in a line: "Passed in 4.2 s against prod, by ann, 2026-11-13 14:02:11". */
export function CheckVerdict({ check }: { check: Schemas["CheckInfo"] }) {
  const Icon = check.ok ? CircleCheck : CircleAlert;
  return (
    <span className="inline-flex flex-wrap items-center gap-1.5 text-sm">
      <Icon aria-hidden className={cn("size-4", check.ok ? "text-ok" : "text-bad")} />
      <strong>{check.ok ? "Passed" : `${count(check.problems)} problem${check.problems === 1 ? "" : "s"}`}</strong>
      <span className="text-muted">
        in {check.seconds.toFixed(1)} s against {check.environment}, by {check.by}, {timestamp(check.at)}
      </span>
    </span>
  );
}

/** The gate's report, as `ductus release check` prints it: each section's lines, with what's under them. */
export function GateReport({ check, verdict = true }: { check: Schemas["CheckInfo"]; verdict?: boolean }) {
  const sections = gateSections(check.report);
  return (
    <section aria-label={`Check of ${check.environment}`} className="space-y-3">
      {verdict && <CheckVerdict check={check} />}
      {sections.length === 0 && <p className="text-sm text-muted">The report has no lines.</p>}
      {sections.map((s, n) => (
        <div key={n}>
          <h4 className="text-xs font-medium tracking-wide text-muted uppercase">
            {sectionTitle(s.section, check.environment)}
          </h4>
          <ul className="mt-1 space-y-1 text-sm">
            {s.checks.map((c, k) => {
              const l = LEVEL[c.level];
              return (
                <li key={k} className="flex gap-1.5">
                  <l.icon aria-label={l.label} className={cn("mt-0.5 size-4 shrink-0", l.className)} />
                  <div className="min-w-0">
                    <div className="break-words">{c.text}</div>
                    {c.more.length > 0 && (
                      <pre className="mt-1 max-h-60 overflow-auto rounded bg-hover p-2 text-xs whitespace-pre-wrap">
                        {c.more.join("\n")}
                      </pre>
                    )}
                  </div>
                </li>
              );
            })}
          </ul>
        </div>
      ))}
    </section>
  );
}
