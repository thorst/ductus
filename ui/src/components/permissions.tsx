import { useEffect } from "react";

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
  deploy: "may store releases and check them here; what they deploy runs with every secret and payload",
} as const;

export type Permission = keyof typeof PERMISSION_HELP;

/** Permissions only operators and admins may hold (the engine refuses them on a viewer). */
export const OPERATORS_ONLY: readonly string[] = ["deploy"];

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

/** Permissions beyond the role, as checkboxes. Admins have them all whatever is ticked; a viewer
 * can't have the operators-only ones (unticked when the role becomes viewer). */
export function PermissionPicker({
  value,
  onChange,
  allowed,
  role,
}: {
  value: string[];
  onChange: (permissions: string[]) => void;
  /** Only these can be given (a token can't have more than its owner). */
  allowed?: string[];
  role: Role;
}) {
  const isAdmin = role === "admin";
  const viewer = role === "viewer";
  useEffect(() => {
    if (viewer && value.some((p) => OPERATORS_ONLY.includes(p))) {
      onChange(value.filter((p) => !OPERATORS_ONLY.includes(p)));
    }
  }, [viewer, value, onChange]);
  const list = (Object.keys(PERMISSION_HELP) as Permission[]).filter((p) => !allowed || allowed.includes(p));
  if (list.length === 0) return null;
  return (
    <fieldset className="text-sm">
      <legend className="text-muted">Permissions</legend>
      <div className="mt-1 space-y-1">
        {list.map((p) => {
          const notForViewers = viewer && OPERATORS_ONLY.includes(p);
          return (
            <label key={p} className="flex items-start gap-2">
              <input
                type="checkbox"
                className="mt-1 size-4"
                checked={isAdmin || (!notForViewers && value.includes(p))}
                disabled={isAdmin || notForViewers}
                onChange={(e) => onChange(e.target.checked ? [...value, p] : value.filter((x) => x !== p))}
              />
              <span>
                <strong>{p}</strong> <span className="text-muted">{PERMISSION_HELP[p]}</span>
                {notForViewers && <span className="text-muted"> (operators and admins only)</span>}
              </span>
            </label>
          );
        })}
        {isAdmin && <p className="text-xs text-muted">Admins have every permission.</p>}
      </div>
    </fieldset>
  );
}
