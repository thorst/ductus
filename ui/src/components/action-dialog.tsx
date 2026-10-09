import { useState, type FormEvent, type ReactNode } from "react";
import { ApiError } from "@/api/client";
import { Button } from "@/components/ui/button";
import { Dialog } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";

/**
 * Confirming an action: what it will do, an optional reason (kept with the action, so the next
 * person sees why), and the engine's own words if it refuses.
 */
export function ActionDialog({
  open,
  onOpenChange,
  title,
  description,
  confirm,
  onConfirm,
  reason = true,
  disabled,
  wide,
  children,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: string;
  description?: ReactNode;
  confirm: string;
  /** Does it; a thrown error is shown and the dialog stays open. */
  onConfirm: (reason: string) => Promise<unknown>;
  /** Ask for a reason (optional to give). */
  reason?: boolean;
  /** Not yet confirmable (e.g. a name still to be typed). */
  disabled?: boolean;
  wide?: boolean;
  children?: ReactNode;
}) {
  const [why, setWhy] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await onConfirm(why.trim());
      setWhy("");
      onOpenChange(false);
    } catch (err) {
      setError(err instanceof ApiError ? err.message : "The engine didn't answer.");
    } finally {
      setBusy(false);
    }
  };
  return (
    <Dialog
      open={open}
      onOpenChange={(o) => {
        if (!o) setError(null);
        onOpenChange(o);
      }}
      title={title}
      description={description}
      wide={wide}
    >
      <form onSubmit={submit} className="space-y-4">
        {children}
        {reason && (
          <label className="block text-sm">
            <span className="text-muted">Reason (optional, kept with the action)</span>
            <Input className="mt-1" value={why} onChange={(e) => setWhy(e.target.value)} />
          </label>
        )}
        {error && (
          <p role="alert" className="rounded-md border border-bad/40 bg-bad/10 p-3 text-sm text-bad">
            {error}
          </p>
        )}
        <div className="flex justify-end gap-2">
          <Button type="button" variant="outline" onClick={() => onOpenChange(false)}>
            Cancel
          </Button>
          <Button type="submit" disabled={busy || disabled}>
            {busy ? "Working…" : confirm}
          </Button>
        </div>
      </form>
    </Dialog>
  );
}
