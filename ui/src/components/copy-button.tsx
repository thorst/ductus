import { Check, Copy } from "lucide-react";
import { useState } from "react";
import { Button } from "@/components/ui/button";

/**
 * Copies `text` and says so. The clipboard needs https or localhost (the UI's own rule too); if
 * the browser refuses, the button says to copy by hand. `what` completes the name for screen
 * readers ("Copy the patch") while the button shows "Copy".
 */
export function CopyButton({ text, what }: { text: string; what: string }) {
  const [state, setState] = useState<"idle" | "copied" | "failed">("idle");
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(text);
      setState("copied");
    } catch {
      setState("failed");
    }
    setTimeout(() => setState("idle"), 2500);
  };
  return (
    <>
      <Button type="button" variant="outline" onClick={() => void copy()}>
        {state === "copied" ? <Check aria-hidden /> : <Copy aria-hidden />}
        Copy<span className="sr-only"> {what}</span>
      </Button>
      <span role="status" className={state === "idle" ? "sr-only" : "self-center text-xs text-muted"}>
        {state === "copied" ? "Copied." : state === "failed" ? "The browser refused: select it and copy by hand." : ""}
      </span>
    </>
  );
}
