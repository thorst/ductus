import { CircleAlert, CircleCheck, CirclePause, TriangleAlert } from "lucide-react";
import { describe, type Tone } from "@/lib/health";
import { cn } from "@/lib/utils";

const TONES: Record<Tone, { icon: typeof CircleCheck; className: string }> = {
  ok: { icon: CircleCheck, className: "text-ok bg-ok/10 border-ok/30" },
  warn: { icon: TriangleAlert, className: "text-warn bg-warn/10 border-warn/40" },
  bad: { icon: CircleAlert, className: "text-bad bg-bad/10 border-bad/40" },
  paused: { icon: CirclePause, className: "text-paused bg-paused/10 border-paused/40" },
};

/** A health state as an icon and a word: color helps, but never says it alone. */
export function HealthBadge({ state, className }: { state: string; className?: string }) {
  const { tone, label } = describe(state);
  const { icon: Icon, className: toneClass } = TONES[tone];
  return (
    <span
      className={cn(
        "inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-xs font-medium whitespace-nowrap",
        toneClass,
        className,
      )}
    >
      <Icon aria-hidden className="size-3.5" />
      {label}
    </span>
  );
}

const STATUS_TONES: Record<string, Tone> = {
  pending: "warn",
  sent: "ok",
  filtered: "paused",
  quarantined: "bad",
  duplicate: "paused",
};

/** A message's or delivery's status. */
export function StatusBadge({ status }: { status: string }) {
  const tone = STATUS_TONES[status] ?? "warn";
  return (
    <span
      className={cn(
        "inline-flex items-center rounded-full border px-2 py-0.5 text-xs font-medium",
        TONES[tone].className,
      )}
    >
      {status}
    </span>
  );
}
