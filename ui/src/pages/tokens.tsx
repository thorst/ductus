import { useQuery } from "@tanstack/react-query";
import { Plus, Trash2 } from "lucide-react";
import { useState } from "react";
import { createToken, revokeToken, useIsAdmin, useRefresh } from "@/api/actions";
import type { Schemas } from "@/api/client";
import { session, tokens } from "@/api/queries";
import { ActionDialog } from "@/components/action-dialog";
import { Loaded, PageHeader } from "@/components/page";
import { PermissionPicker, RolePicker, type Role } from "@/components/permissions";
import { TokenSecret } from "@/components/token-secret";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Table, Td, Th } from "@/components/ui/table";
import { ago, timestamp } from "@/lib/format";

/**
 * API tokens, for scripts and monitoring (and signing in here when `auth.token_sign_in` allows).
 * These are everyone's: service tokens an admin made, and the ones users made for themselves
 * (those belong to their owner and stop when the owner is disabled). A token's secret is shown
 * once, when it's made; the engine keeps only its hash. Revoking one ends it (and its sessions)
 * at once.
 */
export function TokensPage() {
  const isAdmin = useIsAdmin();
  return (
    <>
      <PageHeader title="Tokens">
        For scripts and monitoring. The engine keeps only a token&rsquo;s hash: its secret is shown once.
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
  const [permissions, setPermissions] = useState<string[]>([]);
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
                  <Th>Belongs to</Th>
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
                    <Td>
                      {t.role}
                      {t.permissions.length > 0 && t.role !== "admin" && (
                        <div className="text-xs text-muted">+ {t.permissions.join(", ")}</div>
                      )}
                    </Td>
                    <Td>{t.owner ?? <span className="text-muted">nobody (service)</span>}</Td>
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
        description="A service token, owned by nobody. Name it after what uses it: one token per script, so each can be revoked alone. People make their own under Account."
        confirm="Create"
        disabled={!name.trim()}
        onConfirm={async () => {
          const result = await createToken(name.trim(), role, permissions);
          setName("");
          setRole("viewer");
          setPermissions([]);
          setCreated(result);
          await refresh();
        }}
      >
        <label className="block text-sm">
          <span className="text-muted">Name</span>
          <Input className="mt-1" value={name} placeholder="monitoring" onChange={(e) => setName(e.target.value)} />
        </label>
        <RolePicker value={role} onChange={setRole} />
        <PermissionPicker value={permissions} onChange={setPermissions} isAdmin={role === "admin"} />
      </ActionDialog>
      {created && <TokenSecret created={created} onClose={() => setCreated(null)} />}
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
