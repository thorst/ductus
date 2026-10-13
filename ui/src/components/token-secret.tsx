import { KeyRound } from "lucide-react";
import type { Schemas } from "@/api/client";
import { CopyButton } from "@/components/copy-button";
import { Button } from "@/components/ui/button";
import { Dialog } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";

/** A new token's secret, once: it can't be shown again (only its hash is stored). */
export function TokenSecret({ created, onClose }: { created: Schemas["CreatedToken"]; onClose: () => void }) {
  return (
    <Dialog
      open
      onOpenChange={(open) => !open && onClose()}
      title={`${created.token.name} (${created.token.role})`}
      description="Copy it now and keep it somewhere safe (a secret store, the script's secrets): it's shown only this once."
    >
      <div className="space-y-4">
        <label className="block text-sm">
          <span className="flex items-center gap-1.5 text-muted">
            <KeyRound aria-hidden className="size-4" />
            Token
          </span>
          <Input
            readOnly
            className="mt-1 font-mono text-xs"
            value={created.secret}
            onFocus={(e) => e.target.select()}
          />
        </label>
        <div className="flex flex-wrap justify-end gap-2">
          <CopyButton text={created.secret} what="the token" />
          <Button onClick={onClose}>Done</Button>
        </div>
      </div>
    </Dialog>
  );
}
