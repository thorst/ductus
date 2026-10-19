import { CircleAlert, Mail, UserCheck, Users } from "lucide-react";
import type { Schemas } from "@/api/client";
import { cn } from "@/lib/utils";

type Group = Schemas["OwnerGroup"];

/** The group an interface (or one of its destinations, when it has an owner of its own) belongs to. */
export function ownerGroup(groups: Group[] | undefined, iface: string, destination?: string): Group | undefined {
  return groups?.find((g) => g.owns.some((o) => o.interface === iface && (o.destination ?? undefined) === destination));
}

/** Interfaces the caller owns, or owns a destination of. */
export function myInterfaces(groups: Group[] | undefined): Set<string> {
  return new Set((groups ?? []).filter((g) => g.you).flatMap((g) => g.owns.map((o) => o.interface)));
}

/** Owners that reach nobody or the wrong person (what `ductus owners --check` refuses). */
export function unreachable(group: Group): boolean {
  return group.reach === "nobody" || group.reach === "disabled" || group.reach === "several";
}

/**
 * Who an owner is: the person with an account (name, email), a team, an address, or a warning that
 * it reaches nobody. Words and an icon, never color alone.
 */
export function OwnerReach({ group, className }: { group: Group; className?: string }) {
  const p = group.person;
  const name = p?.display_name ?? p?.username;
  const line = "inline-flex flex-wrap items-center gap-x-1.5 gap-y-0.5";
  switch (group.reach) {
    case "user":
      return (
        <span className={cn(line, className)}>
          <UserCheck aria-hidden className="size-3.5 shrink-0" />
          <span>
            {name}
            {p?.display_name && <span className="text-muted"> ({p.username})</span>}
            {group.you && <span className="font-semibold"> · you</span>}
          </span>
          {p?.email && (
            <a href={`mailto:${p.email}`} className="text-xs">
              {p.email}
            </a>
          )}
        </span>
      );
    case "team":
      return (
        <span className={cn(line, "text-muted", className)}>
          <Users aria-hidden className="size-3.5 shrink-0" />
          Team (<code>owners:</code> in ductus.yaml), not a user
        </span>
      );
    case "address":
      return (
        <span className={cn(line, "text-muted", className)}>
          <Mail aria-hidden className="size-3.5 shrink-0" />
          An email address, not a user
        </span>
      );
    case "disabled":
      return (
        <span className={cn(line, "text-warn", className)}>
          <CircleAlert aria-hidden className="size-3.5 shrink-0" />
          Account {p?.username} is disabled: move what they own
        </span>
      );
    case "several":
      return (
        <span className={cn(line, "text-warn", className)}>
          <CircleAlert aria-hidden className="size-3.5 shrink-0" />
          Names several accounts: use a username or an email
        </span>
      );
    case "nobody":
      return (
        <span className={cn(line, "text-warn", className)}>
          <CircleAlert aria-hidden className="size-3.5 shrink-0" />
          Not a user, not a team in <code>owners:</code>
        </span>
      );
    default:
      return null;
  }
}
