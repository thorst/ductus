import { useQuery } from "@tanstack/react-query";
import { KeyRound, Plus, Trash2 } from "lucide-react";
import { useState } from "react";
import { createToken, revokeToken, useIsAdmin, useRefresh } from "@/api/actions";
import type { Schemas } from "@/api/client";
import { session, tokens } from "@/api/queries";
import { ActionDialog } from "@/components/action-dialog";
import { CopyButton } from "@/components/copy-button";
import { Loaded, PageHeader } from "@/components/page";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Dialog } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Table, Td, Th } from "@/components/ui/table";
import { ago, timestamp } from "@/lib/format";

const ROLES = {
  viewer: "reads everything, payloads included",
  operator: "also resends, pauses, drains, confirms retention, runs connection checks",
  admin: "also manages tokens",
} as const;

type Role = keyof typeof ROLES;

/**
 * API tokens, for scripts, monitoring and signing in here. A token's secret is shown once, when
 * it's made; the engine keeps only its hash. Revoking one ends it (and its sessions) at once.
 */
export function TokensPage() {
  const isAdmin = useIsAdmin();
  return (
    <>
      <PageHeader title="Tokens">
        For scripts, monitoring and signing in here. The engine keeps only a token's hash: its secret is shown once.
      </PageHeader>
      {isAdmin ? <Tokens /> : <p className="text-sm text-muted">Managing tokens needs an admin token.</p>}
    </>
  );
}

function Tokens() {
  const query = useQuery(tokens);
  const me = useQuery(session);
  const refresh = useRefresh();
  const [creating, setCreating] = useState(false);
  const [name, setName] = useState("");
  const [role, setRole] = useState<Role>("viewer");
  const [created, setCreated] = useState<Schemas["CreatedToken"] | null>(null);
  const [revoking, setRevoking] = useState<Schemas["TokenInfo"] | null>(null);
  return (
    <>
      <div className="mb-4 flex justify-end">
        <Button onClick={() => setCreating(true)}>
          <Plus aria-hidden />
          New token
        </Button>
      </div>
      <Loaded query={query}>
        {(list) => (
          <Card>
            <Table aria-label="Tokens">
              <thead>
                <tr>
                  <Th>Name</Th>
                  <Th>Role</Th>
                  <Th>Created</Th>
                  <Th>Last used</Th>
                  <Th className="w-0" />
                </tr>
              </thead>
              <tbody>
                {list.map((t) => (
                  <tr key={t.name}>
                    <Td className="font-medium">
                      {t.name}
                      {t.name === me.data?.name && <span className="ml-2 text-xs text-muted">(you)</span>}
                    </Td>
                    <Td>{t.role}</Td>
                    <Td className="tabular-nums">{timestamp(t.created_at)}</Td>
                    <Td>{t.last_used_at ? ago(t.last_used_at) : "never"}</Td>
                    <Td>
                      <Button size="sm" variant="ghost" onClick={() => setRevoking(t)}>
                        <Trash2 aria-hidden />
                        Revoke<span className="sr-only"> {t.name}</span>
                      </Button>
                    </Td>
                  </tr>
                ))}
              </tbody>
            </Table>
          </Card>
        )}
      </Loaded>
      <ActionDialog
        open={creating}
        onOpenChange={setCreating}
        reason={false}
        title="New token"
        description="Name it after who or what uses it: one token per script or service, so each can be revoked alone."
        confirm="Create"
        disabled={!name.trim()}
        onConfirm={async () => {
          const result = await createToken(name.trim(), role);
          setName("");
          setRole("viewer");
          setCreated(result);
          await refresh();
        }}
      >
        <label className="block text-sm">
          <span className="text-muted">Name</span>
          <Input className="mt-1" value={name} placeholder="monitoring" onChange={(e) => setName(e.target.value)} />
        </label>
        <fieldset className="text-sm">
          <legend className="text-muted">Role</legend>
          <div className="mt-1 space-y-1">
            {(Object.keys(ROLES) as Role[]).map((r) => (
              <label key={r} className="flex items-start gap-2">
                <input type="radio" name="role" className="mt-1" checked={role === r} onChange={() => setRole(r)} />
                <span>
                  <strong>{r}</strong> <span className="text-muted">{ROLES[r]}</span>
                </span>
              </label>
            ))}
          </div>
        </fieldset>
      </ActionDialog>
      {created && <Secret created={created} onClose={() => setCreated(null)} />}
      {revoking && (
        <ActionDialog
          open
          onOpenChange={(open) => !open && setRevoking(null)}
          reason={false}
          title={`Revoke ${revoking.name}`}
          description={
            revoking.name === me.data?.name
              ? "This is the token you signed in with: you'll be signed out at once."
              : "It stops working at once, and every session signed in with it ends. Scripts using it get 401."
          }
          confirm="Revoke"
          onConfirm={async () => {
            await revokeToken(revoking.name);
            await refresh();
          }}
        />
      )}
    </>
  );
}

/** The secret, once: it can't be shown again (only its hash is stored). */
function Secret({ created, onClose }: { created: Schemas["CreatedToken"]; onClose: () => void }) {
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
