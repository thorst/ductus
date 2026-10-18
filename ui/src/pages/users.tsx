import { useQuery } from "@tanstack/react-query";
import { KeyRound, Lock, LockOpen, Network, Pencil, Plus, UserRoundX, UserRoundCheck } from "lucide-react";
import { useState } from "react";
import { createUser, resetPassword, unlockUser, updateUser, useIsAdmin, useRefresh } from "@/api/actions";
import type { Schemas } from "@/api/client";
import { session, users } from "@/api/queries";
import { ActionDialog } from "@/components/action-dialog";
import { Loaded, PageHeader } from "@/components/page";
import { PermissionPicker, RolePicker, type Role } from "@/components/permissions";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Table, Td, Th } from "@/components/ui/table";
import { ago, timestamp } from "@/lib/format";

/**
 * The people who sign in: admins make accounts (with a temporary password the person changes at
 * their first sign-in), set roles and permissions, reset passwords, unlock accounts locked by
 * wrong passwords, and disable people who leave (their sessions and tokens stop at once).
 */
export function UsersPage() {
  const isAdmin = useIsAdmin();
  return (
    <>
      <PageHeader title="Users">
        Who signs in, and what each may do. Accounts are disabled, never deleted: the record of what they did names
        them. Directory accounts are made at their first sign-in; their role, name and email come from the directory.
      </PageHeader>
      {isAdmin ? <Users /> : <p className="text-sm text-muted">Managing users needs an admin.</p>}
    </>
  );
}

type Dialog =
  | { kind: "new" }
  | { kind: "edit" | "password"; user: Schemas["UserInfo"] }
  | { kind: "disable"; user: Schemas["UserInfo"] };

function Users() {
  const query = useQuery(users);
  const me = useQuery(session);
  const refresh = useRefresh();
  const [dialog, setDialog] = useState<Dialog | null>(null);
  return (
    <>
      <div className="mb-4 flex justify-end">
        <Button onClick={() => setDialog({ kind: "new" })}>
          <Plus aria-hidden />
          New user
        </Button>
      </div>
      <Loaded query={query}>
        {(list) => (
          <Card>
            <Table aria-label="Users">
              <thead>
                <tr>
                  <Th>User</Th>
                  <Th>Role</Th>
                  <Th>State</Th>
                  <Th>Last sign-in</Th>
                  <Th className="w-0">Actions</Th>
                </tr>
              </thead>
              <tbody>
                {list.map((u) => (
                  <tr key={u.id}>
                    <Td>
                      <span className="font-medium">{u.username}</span>
                      {u.username.toLowerCase() === me.data?.name.toLowerCase() && (
                        <span className="ml-2 text-xs text-muted">(you)</span>
                      )}
                      {(u.display_name || u.email) && (
                        <div className="text-xs text-muted">
                          {[u.display_name, u.email].filter(Boolean).join(" · ")}
                        </div>
                      )}
                      <div className="text-xs text-muted">
                        {u.source === "ldap" ? (
                          <span className="inline-flex items-center gap-1">
                            <Network aria-hidden className="size-3" />
                            Directory account
                            {u.directory_checked_at && <>, checked {ago(u.directory_checked_at)}</>}
                          </span>
                        ) : (
                          "Local account"
                        )}
                      </div>
                    </Td>
                    <Td>
                      {u.role}
                      {u.permissions.length > 0 && u.role !== "admin" && (
                        <div className="text-xs text-muted">+ {u.permissions.join(", ")}</div>
                      )}
                    </Td>
                    <Td>
                      <State user={u} />
                    </Td>
                    <Td>{u.last_sign_in_at ? ago(u.last_sign_in_at) : "never"}</Td>
                    <Td>
                      <div className="flex flex-wrap gap-1">
                        {/* A directory account's role, name and email are the directory's: nothing to edit here. */}
                        {u.source === "local" && (
                          <>
                            <Button size="sm" variant="ghost" onClick={() => setDialog({ kind: "edit", user: u })}>
                              <Pencil aria-hidden />
                              Edit<span className="sr-only"> {u.username}</span>
                            </Button>
                            <Button size="sm" variant="ghost" onClick={() => setDialog({ kind: "password", user: u })}>
                              <KeyRound aria-hidden />
                              Reset password<span className="sr-only"> for {u.username}</span>
                            </Button>
                          </>
                        )}
                        {u.locked_until && (
                          <Button
                            size="sm"
                            variant="ghost"
                            onClick={() => void unlockUser(u.id).then(refresh)}
                            title="Let them try again"
                          >
                            <LockOpen aria-hidden />
                            Unlock<span className="sr-only"> {u.username}</span>
                          </Button>
                        )}
                        <Button
                          size="sm"
                          variant="ghost"
                          onClick={() =>
                            u.disabled
                              ? void updateUser(u.id, { disabled: false }).then(refresh)
                              : setDialog({ kind: "disable", user: u })
                          }
                        >
                          {u.disabled ? <UserRoundCheck aria-hidden /> : <UserRoundX aria-hidden />}
                          {u.disabled ? "Enable" : "Disable"}
                          <span className="sr-only"> {u.username}</span>
                        </Button>
                      </div>
                    </Td>
                  </tr>
                ))}
              </tbody>
            </Table>
          </Card>
        )}
      </Loaded>
      {dialog?.kind === "new" && <NewUser onClose={() => setDialog(null)} />}
      {dialog?.kind === "edit" && <EditUser user={dialog.user} onClose={() => setDialog(null)} />}
      {dialog?.kind === "password" && <ResetPassword user={dialog.user} onClose={() => setDialog(null)} />}
      {dialog?.kind === "disable" && (
        <ActionDialog
          open
          onOpenChange={(open) => !open && setDialog(null)}
          reason={false}
          title={`Disable ${dialog.user.username}`}
          description={
            dialog.user.username.toLowerCase() === me.data?.name.toLowerCase()
              ? "This is you: you'll be signed out at once."
              : "They are signed out at once, their tokens stop working, and they can't sign in until you enable them again."
          }
          confirm="Disable"
          onConfirm={async () => {
            await updateUser(dialog.user.id, { disabled: true });
            await refresh();
          }}
        />
      )}
    </>
  );
}

/** The state in words and an icon: never color alone. */
function State({ user }: { user: Schemas["UserInfo"] }) {
  if (user.disabled)
    return (
      <span className="inline-flex items-center gap-1 text-muted">
        <UserRoundX aria-hidden className="size-4" />
        Disabled
      </span>
    );
  if (user.locked_until)
    return (
      <span className="inline-flex items-center gap-1 text-bad">
        <Lock aria-hidden className="size-4" />
        Locked until {timestamp(user.locked_until)}
      </span>
    );
  if (user.directory_refused)
    return (
      <span className="inline-flex items-center gap-1 text-warn" title="In no Ductus group, disabled or gone there">
        <UserRoundX aria-hidden className="size-4" />
        Refused by the directory
      </span>
    );
  if (user.must_change_password) return <span className="text-warn">Must change password</span>;
  return <span>Active</span>;
}

function NewUser({ onClose }: { onClose: () => void }) {
  const refresh = useRefresh();
  const [username, setUsername] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [email, setEmail] = useState("");
  const [role, setRole] = useState<Role>("viewer");
  const [permissions, setPermissions] = useState<string[]>([]);
  const [password, setPassword] = useState("");
  return (
    <ActionDialog
      open
      onOpenChange={(open) => !open && onClose()}
      reason={false}
      wide
      title="New user"
      description="Give them the temporary password yourself. They are asked to choose their own when they first sign in."
      confirm="Create"
      disabled={!username.trim() || !password}
      onConfirm={async () => {
        await createUser({
          username: username.trim(),
          display_name: displayName.trim(),
          email: email.trim(),
          role,
          permissions,
          password,
        });
        await refresh();
      }}
    >
      <label className="block text-sm">
        <span className="text-muted">Username</span>
        <Input className="mt-1" autoComplete="off" value={username} onChange={(e) => setUsername(e.target.value)} />
      </label>
      <div className="grid gap-3 sm:grid-cols-2">
        <label className="block text-sm">
          <span className="text-muted">Name</span>
          <Input className="mt-1" value={displayName} onChange={(e) => setDisplayName(e.target.value)} />
        </label>
        <label className="block text-sm">
          <span className="text-muted">Email (alerts go here)</span>
          <Input className="mt-1" type="email" value={email} onChange={(e) => setEmail(e.target.value)} />
        </label>
      </div>
      <RolePicker value={role} onChange={setRole} name="new-user-role" />
      <PermissionPicker value={permissions} onChange={setPermissions} isAdmin={role === "admin"} />
      <label className="block text-sm">
        <span className="text-muted">Temporary password</span>
        <Input
          className="mt-1"
          type="password"
          autoComplete="new-password"
          value={password}
          onChange={(e) => setPassword(e.target.value)}
        />
      </label>
    </ActionDialog>
  );
}

function EditUser({ user, onClose }: { user: Schemas["UserInfo"]; onClose: () => void }) {
  const refresh = useRefresh();
  const [displayName, setDisplayName] = useState(user.display_name);
  const [email, setEmail] = useState(user.email);
  const [role, setRole] = useState<Role>(user.role as Role);
  const [permissions, setPermissions] = useState<string[]>(user.permissions);
  return (
    <ActionDialog
      open
      onOpenChange={(open) => !open && onClose()}
      reason={false}
      wide
      title={`Edit ${user.username}`}
      confirm="Save"
      onConfirm={async () => {
        await updateUser(user.id, { display_name: displayName.trim(), email: email.trim(), role, permissions });
        await refresh();
      }}
    >
      <div className="grid gap-3 sm:grid-cols-2">
        <label className="block text-sm">
          <span className="text-muted">Name</span>
          <Input className="mt-1" value={displayName} onChange={(e) => setDisplayName(e.target.value)} />
        </label>
        <label className="block text-sm">
          <span className="text-muted">Email (alerts go here)</span>
          <Input className="mt-1" type="email" value={email} onChange={(e) => setEmail(e.target.value)} />
        </label>
      </div>
      <RolePicker value={role} onChange={setRole} name="edit-user-role" />
      <PermissionPicker value={permissions} onChange={setPermissions} isAdmin={role === "admin"} />
    </ActionDialog>
  );
}

function ResetPassword({ user, onClose }: { user: Schemas["UserInfo"]; onClose: () => void }) {
  const refresh = useRefresh();
  const [password, setPassword] = useState("");
  return (
    <ActionDialog
      open
      onOpenChange={(open) => !open && onClose()}
      reason={false}
      title={`Reset the password of ${user.username}`}
      description="Their sessions end now. They choose their own password when they next sign in."
      confirm="Reset"
      disabled={!password}
      onConfirm={async () => {
        await resetPassword(user.id, password);
        await refresh();
      }}
    >
      <label className="block text-sm">
        <span className="text-muted">Temporary password</span>
        <Input
          className="mt-1"
          type="password"
          autoComplete="new-password"
          value={password}
          onChange={(e) => setPassword(e.target.value)}
        />
      </label>
    </ActionDialog>
  );
}
