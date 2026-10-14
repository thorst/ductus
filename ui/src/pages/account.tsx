import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate } from "@tanstack/react-router";
import { Palette, Plus, Trash2 } from "lucide-react";
import { useState, type FormEvent } from "react";
import {
  changePassword,
  endOtherSessions,
  endSession,
  makeMyToken,
  revokeMyToken,
  updateProfile,
  useRefresh,
} from "@/api/actions";
import { ApiError, type Schemas } from "@/api/client";
import { account, myActivity, myTokens, mySessions, session } from "@/api/queries";
import { ActionDialog } from "@/components/action-dialog";
import { AuditTable } from "@/components/audit-table";
import { Loaded, PageHeader } from "@/components/page";
import { PERMISSION_HELP, PermissionPicker, RolePicker, ROLES, type Role } from "@/components/permissions";
import { TokenSecret } from "@/components/token-secret";
import { Button } from "@/components/ui/button";
import { Card, CardBody, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Table, Td, Th } from "@/components/ui/table";
import { ago, timestamp } from "@/lib/format";

/**
 * Your own account: details, password, signed-in browsers, tokens of your own, and where the
 * appearance is kept. A session made with a token has no account, and says so. When an admin
 * chose your password, this is the only page that works until you change it.
 */
export function AccountPage() {
  const { data: me } = useQuery(session);
  if (!me) return null;
  if (me.kind !== "user") {
    return (
      <>
        <PageHeader title="Account" />
        <p className="max-w-prose text-sm text-muted">
          You&rsquo;re signed in with the API token <strong className="text-text">{me.name}</strong>({me.role}). Tokens
          have no account to keep a password, sessions or appearance in: sign in with a username and password for that.
          Your theme and colors are kept in this browser only.
        </p>
      </>
    );
  }
  return (
    <>
      <PageHeader title="Account">
        {me.name} · {me.role}
      </PageHeader>
      {me.must_change_password && (
        <p role="note" className="mb-4 rounded-md border border-warn/50 bg-warn/10 p-3 text-sm">
          An admin chose your password. Choose your own to continue: nothing else works until you do.
        </p>
      )}
      <div className="max-w-3xl space-y-6">
        <PasswordCard mustChange={me.must_change_password} />
        {!me.must_change_password && (
          <>
            <ProfileCard />
            <PermissionsCard me={me} />
            <SessionsCard />
            <TokensCard me={me} />
            <ActivityCard />
            <AppearanceCard />
          </>
        )}
      </div>
    </>
  );
}

/** What you did yourself (and what your tokens did), from the audit log: anyone may read their own. */
function ActivityCard() {
  const query = useQuery(myActivity);
  return (
    <Card aria-label="My activity">
      <CardHeader>
        <CardTitle>My activity</CardTitle>
      </CardHeader>
      <Loaded query={query}>
        {(page) =>
          page.entries.length === 0 ? (
            <CardBody>
              <p className="text-sm text-muted">Nothing yet.</p>
            </CardBody>
          ) : (
            <>
              <AuditTable entries={page.entries} whose={false} label="My activity" />
              <p className="p-3 text-xs text-muted">
                The latest {page.entries.length}. Admins and people with the audit permission can read the whole log.
              </p>
            </>
          )
        }
      </Loaded>
    </Card>
  );
}

function PasswordCard({ mustChange }: { mustChange: boolean }) {
  const [current, setCurrent] = useState("");
  const [next, setNext] = useState("");
  const [again, setAgain] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState(false);
  const [busy, setBusy] = useState(false);
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const mismatch = again !== "" && again !== next;
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    setDone(false);
    try {
      await changePassword(current, next);
      setCurrent("");
      setNext("");
      setAgain("");
      setDone(true);
      await queryClient.invalidateQueries({ queryKey: ["session"] });
      if (mustChange) await navigate({ to: "/" });
    } catch (err) {
      setError(err instanceof ApiError ? err.message : "The engine didn't answer.");
    } finally {
      setBusy(false);
    }
  };
  return (
    <Card aria-label="Password">
      <CardHeader>
        <CardTitle>Password</CardTitle>
      </CardHeader>
      <CardBody>
        <form onSubmit={submit} className="max-w-sm space-y-3">
          <label className="block text-sm">
            <span className="text-muted">Current password</span>
            <Input
              type="password"
              autoComplete="current-password"
              required
              className="mt-1"
              value={current}
              onChange={(e) => setCurrent(e.target.value)}
            />
          </label>
          <label className="block text-sm">
            <span className="text-muted">New password</span>
            <Input
              type="password"
              autoComplete="new-password"
              required
              className="mt-1"
              value={next}
              onChange={(e) => setNext(e.target.value)}
            />
          </label>
          <label className="block text-sm">
            <span className="text-muted">New password again</span>
            <Input
              type="password"
              autoComplete="new-password"
              required
              className="mt-1"
              value={again}
              onChange={(e) => setAgain(e.target.value)}
              aria-invalid={mismatch}
            />
          </label>
          {mismatch && <p className="text-sm text-bad">The two don&rsquo;t match.</p>}
          {error && (
            <p role="alert" className="rounded-md border border-bad/40 bg-bad/10 p-3 text-sm text-bad">
              {error}
            </p>
          )}
          {done && (
            <p role="status" className="text-sm text-ok">
              Password changed. Your other sessions were signed out.
            </p>
          )}
          <Button type="submit" disabled={busy || !current || !next || next !== again}>
            {busy ? "Changing…" : "Change password"}
          </Button>
        </form>
      </CardBody>
    </Card>
  );
}

function ProfileCard() {
  const query = useQuery(account);
  const refresh = useRefresh();
  return (
    <Card aria-label="Details">
      <CardHeader>
        <CardTitle>Details</CardTitle>
      </CardHeader>
      <CardBody>
        <Loaded query={query}>{(user) => <ProfileForm key={user.id} user={user} onSaved={refresh} />}</Loaded>
      </CardBody>
    </Card>
  );
}

function ProfileForm({ user, onSaved }: { user: Schemas["UserInfo"]; onSaved: () => Promise<unknown> }) {
  const [name, setName] = useState(user.display_name);
  const [email, setEmail] = useState(user.email);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setError(null);
    setSaved(false);
    try {
      await updateProfile({ display_name: name, email });
      setSaved(true);
      await onSaved();
    } catch (err) {
      setError(err instanceof ApiError ? err.message : "The engine didn't answer.");
    }
  };
  return (
    <form onSubmit={submit} className="max-w-sm space-y-3">
      <label className="block text-sm">
        <span className="text-muted">Name</span>
        <Input className="mt-1" value={name} onChange={(e) => setName(e.target.value)} />
      </label>
      <label className="block text-sm">
        <span className="text-muted">Email (where alerts for you go)</span>
        <Input className="mt-1" type="email" value={email} onChange={(e) => setEmail(e.target.value)} />
      </label>
      {error && (
        <p role="alert" className="rounded-md border border-bad/40 bg-bad/10 p-3 text-sm text-bad">
          {error}
        </p>
      )}
      {saved && (
        <p role="status" className="text-sm text-ok">
          Saved.
        </p>
      )}
      <Button type="submit" variant="outline">
        Save
      </Button>
    </form>
  );
}

function PermissionsCard({ me }: { me: Schemas["Session"] }) {
  const has = (p: keyof typeof PERMISSION_HELP) => me.permissions.includes(p);
  return (
    <Card aria-label="What you can do">
      <CardHeader>
        <CardTitle>What you can do</CardTitle>
      </CardHeader>
      <CardBody>
        <ul className="space-y-1 text-sm">
          <li>
            Role <strong>{me.role}</strong>.
          </li>
          {(Object.keys(PERMISSION_HELP) as (keyof typeof PERMISSION_HELP)[]).map((p) => (
            <li key={p}>
              <strong>{p}</strong>: {has(p) ? "yes, you " : "no, you may not "}
              {PERMISSION_HELP[p]}.
            </li>
          ))}
        </ul>
        <p className="mt-3 text-xs text-muted">An admin changes these.</p>
      </CardBody>
    </Card>
  );
}

function SessionsCard() {
  const query = useQuery(mySessions);
  const refresh = useRefresh();
  const [error, setError] = useState<string | null>(null);
  const run = async (action: () => Promise<unknown>) => {
    setError(null);
    try {
      await action();
      await refresh();
    } catch (err) {
      setError(err instanceof ApiError ? err.message : "The engine didn't answer.");
    }
  };
  return (
    <Card aria-label="Signed-in browsers">
      <CardHeader>
        <CardTitle>Signed-in browsers</CardTitle>
      </CardHeader>
      <Loaded query={query}>
        {(list) => (
          <>
            <Table aria-label="Sessions">
              <thead>
                <tr>
                  <Th>Signed in</Th>
                  <Th>Last used</Th>
                  <Th>Ends at the latest</Th>
                  <Th className="w-0" />
                </tr>
              </thead>
              <tbody>
                {list.map((s) => (
                  <tr key={s.id}>
                    <Td className="tabular-nums">
                      {timestamp(s.created_at)}
                      {s.current && <span className="ml-2 text-xs text-muted">(this browser)</span>}
                    </Td>
                    <Td>{ago(s.last_seen_at)}</Td>
                    <Td className="tabular-nums">{timestamp(s.expires_at)}</Td>
                    <Td>
                      {!s.current && (
                        <Button size="sm" variant="ghost" onClick={() => void run(() => endSession(s.id))}>
                          Sign out<span className="sr-only"> the session from {timestamp(s.created_at)}</span>
                        </Button>
                      )}
                    </Td>
                  </tr>
                ))}
              </tbody>
            </Table>
            <CardBody>
              {error && (
                <p role="alert" className="mb-3 rounded-md border border-bad/40 bg-bad/10 p-3 text-sm text-bad">
                  {error}
                </p>
              )}
              <Button
                variant="outline"
                disabled={list.length < 2}
                onClick={() => void run(endOtherSessions)}
                title="Ends every session of yours but this one"
              >
                Sign out everywhere else
              </Button>
            </CardBody>
          </>
        )}
      </Loaded>
    </Card>
  );
}

function TokensCard({ me }: { me: Schemas["Session"] }) {
  const query = useQuery(myTokens);
  const refresh = useRefresh();
  const [creating, setCreating] = useState(false);
  const [name, setName] = useState("");
  const [role, setRole] = useState<Role>(me.role as Role);
  const [permissions, setPermissions] = useState<string[]>([]);
  const [created, setCreated] = useState<Schemas["CreatedToken"] | null>(null);
  const [revoking, setRevoking] = useState<Schemas["TokenInfo"] | null>(null);
  const roles = ROLES.slice(0, ROLES.indexOf(me.role as Role) + 1);
  return (
    <Card aria-label="Your tokens">
      <CardHeader>
        <CardTitle>Your tokens</CardTitle>
      </CardHeader>
      <Loaded query={query}>
        {(list) => (
          <>
            {list.length > 0 && (
              <Table aria-label="Tokens">
                <thead>
                  <tr>
                    <Th>Name</Th>
                    <Th>Role</Th>
                    <Th>Last used</Th>
                    <Th className="w-0" />
                  </tr>
                </thead>
                <tbody>
                  {list.map((t) => (
                    <tr key={t.name}>
                      <Td className="font-medium">{t.name}</Td>
                      <Td>
                        {t.role}
                        {t.permissions.length > 0 && <span className="text-muted"> + {t.permissions.join(", ")}</span>}
                      </Td>
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
            )}
            <CardBody>
              <p className="mb-3 text-sm text-muted">
                For your scripts. A token acts as you at most: never above your role or permissions, and it stops
                working if you are disabled.
              </p>
              <Button variant="outline" onClick={() => setCreating(true)}>
                <Plus aria-hidden />
                New token
              </Button>
            </CardBody>
          </>
        )}
      </Loaded>
      <ActionDialog
        open={creating}
        onOpenChange={setCreating}
        reason={false}
        title="New token of yours"
        description="Name it after what uses it. It is listed as your name and this name together."
        confirm="Create"
        disabled={!name.trim()}
        onConfirm={async () => {
          const result = await makeMyToken(name.trim(), role, permissions);
          setName("");
          setPermissions([]);
          setCreated(result);
          await refresh();
        }}
      >
        <label className="block text-sm">
          <span className="text-muted">Name</span>
          <Input className="mt-1" value={name} placeholder="deploy-script" onChange={(e) => setName(e.target.value)} />
        </label>
        <RolePicker value={role} onChange={setRole} roles={roles} name="my-token-role" />
        <PermissionPicker
          value={permissions}
          onChange={setPermissions}
          allowed={me.role === "admin" ? undefined : me.permissions}
          isAdmin={role === "admin"}
        />
      </ActionDialog>
      {created && <TokenSecret created={created} onClose={() => setCreated(null)} />}
      {revoking && (
        <ActionDialog
          open
          onOpenChange={(open) => !open && setRevoking(null)}
          reason={false}
          title={`Revoke ${revoking.name}`}
          description="It stops working at once: scripts using it get 401."
          confirm="Revoke"
          onConfirm={async () => {
            await revokeMyToken(revoking.name);
            await refresh();
          }}
        />
      )}
    </Card>
  );
}

function AppearanceCard() {
  return (
    <Card aria-label="Appearance">
      <CardHeader>
        <CardTitle>Appearance</CardTitle>
      </CardHeader>
      <CardBody>
        <p className="mb-3 text-sm text-muted">
          Your theme and colors are saved to your account, so they follow you to any browser.
        </p>
        <Button asChild variant="outline">
          <Link to="/appearance">
            <Palette aria-hidden />
            Choose theme and colors
          </Link>
        </Button>
      </CardBody>
    </Card>
  );
}
