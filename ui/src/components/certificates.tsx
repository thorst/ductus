import { CircleAlert, CircleCheck, CircleHelp, TriangleAlert } from "lucide-react";
import { countdown, type Countdown } from "@/lib/certificates";
import { cn } from "@/lib/utils";

const TONES = {
  bad: { icon: CircleAlert, className: "text-bad" },
  warn: { icon: TriangleAlert, className: "text-warn" },
  ok: { icon: CircleCheck, className: "text-ok" },
  unknown: { icon: CircleHelp, className: "text-muted" },
};

/** Days left as an icon and words ("14 days left", "Expired 2 days ago"): never color alone. */
export function CertificateDays({ certificate }: { certificate: Countdown }) {
  const { text, tone } = countdown(certificate);
  const t = TONES[tone];
  return (
    <span className={cn("inline-flex items-center gap-1 font-medium whitespace-nowrap", t.className)}>
      <t.icon aria-hidden className="size-4 shrink-0" />
      {text}
    </span>
  );
}
