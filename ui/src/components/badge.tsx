import type { ReactNode } from "react";
import { cn } from "@/lib/utils";

/** A small rounded label: "Running", an environment's name. */
export function Badge({ className, children }: { className?: string; children: ReactNode }) {
  return <span className={cn("rounded-full border px-2 py-0.5 text-xs whitespace-nowrap", className)}>{children}</span>;
}

/** The environments a release went to, as badges ("nowhere yet" when none). */
export function Environments({ names }: { names: string[] }) {
  if (names.length === 0) return <span className="text-sm text-muted">nowhere yet</span>;
  return (
    <span className="flex flex-wrap gap-1">
      {names.map((n) => (
        <Badge key={n}>{n}</Badge>
      ))}
    </span>
  );
}
