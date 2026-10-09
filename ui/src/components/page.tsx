import type { ReactNode } from "react";
import { ApiError } from "@/api/client";

export function PageHeader({
  title,
  children,
  actions,
}: {
  title: ReactNode;
  children?: ReactNode;
  actions?: ReactNode;
}) {
  return (
    <div className="mb-6 flex flex-wrap items-start justify-between gap-4">
      <div className="min-w-0">
        <h1 className="text-xl font-semibold tracking-tight">{title}</h1>
        {children && <div className="mt-1 text-sm text-muted">{children}</div>}
      </div>
      {actions}
    </div>
  );
}

/** Loading, failed, or the content. */
export function Loaded<T>({
  query,
  children,
}: {
  query: { data?: T; error: Error | null };
  children: (data: T) => ReactNode;
}) {
  if (query.error) {
    const message = query.error instanceof ApiError ? query.error.message : "The engine didn't answer.";
    return (
      <p role="alert" className="rounded-md border border-bad/40 bg-bad/10 p-4 text-sm text-bad">
        {message}
      </p>
    );
  }
  if (query.data === undefined) return <p className="text-sm text-muted">Loading…</p>;
  return <>{children(query.data)}</>;
}

/** A label and its value, for detail lists. */
export function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="min-w-0">
      <dt className="text-xs text-muted">{label}</dt>
      <dd className="mt-0.5 text-sm break-words">{children}</dd>
    </div>
  );
}
