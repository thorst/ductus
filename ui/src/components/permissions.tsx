export const ROLE_HELP = {
  viewer: "looks at everything but changes nothing",
  operator: "also resends and corrects messages, pauses, drains, confirms retention, runs connection checks",
  admin: "also manages users and tokens, and has every permission",
} as const;

export type Role = keyof typeof ROLE_HELP;

export const ROLES = Object.keys(ROLE_HELP) as Role[];

export const PERMISSION_HELP = {
  payloads: "may read message payloads (patient data)",
  audit: "may read the audit log",
} as const;

export type Permission = keyof typeof PERMISSION_HELP;

/** Role as radio buttons. */
export function RolePicker({
  value,
  onChange,
  roles = ROLES,
  name = "role",
}: {
  value: Role;
  onChange: (role: Role) => void;
  roles?: Role[];
  name?: string;
}) {
  return (
    <fieldset className="text-sm">
      <legend className="text-muted">Role</legend>
      <div className="mt-1 space-y-1">
        {roles.map((r) => (
          <label key={r} className="flex items-start gap-2">
            <input type="radio" name={name} className="mt-1" checked={value === r} onChange={() => onChange(r)} />
            <span>
              <strong>{r}</strong> <span className="text-muted">{ROLE_HELP[r]}</span>
            </span>
          </label>
        ))}
      </div>
    </fieldset>
  );
}

/** Permissions beyond the role, as checkboxes. Admins have them all whatever is ticked. */
export function PermissionPicker({
  value,
  onChange,
  allowed,
  isAdmin,
}: {
  value: string[];
  onChange: (permissions: string[]) => void;
  /** Only these can be given (a token can't have more than its owner). */
  allowed?: string[];
  isAdmin?: boolean;
}) {
  const list = (Object.keys(PERMISSION_HELP) as Permission[]).filter((p) => !allowed || allowed.includes(p));
  if (list.length === 0) return null;
  return (
    <fieldset className="text-sm">
      <legend className="text-muted">Permissions</legend>
      <div className="mt-1 space-y-1">
        {list.map((p) => (
          <label key={p} className="flex items-start gap-2">
            <input
              type="checkbox"
              className="mt-1 size-4"
              checked={isAdmin || value.includes(p)}
              disabled={isAdmin}
              onChange={(e) => onChange(e.target.checked ? [...value, p] : value.filter((x) => x !== p))}
            />
            <span>
              <strong>{p}</strong> <span className="text-muted">{PERMISSION_HELP[p]}</span>
            </span>
          </label>
        ))}
        {isAdmin && <p className="text-xs text-muted">Admins have every permission.</p>}
      </div>
    </fieldset>
  );
}
