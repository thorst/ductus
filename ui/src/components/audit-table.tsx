import { Link } from "@tanstack/react-router";
import type { Schemas } from "@/api/client";
import { Table, Td, Th } from "@/components/ui/table";
import { ms, timestamp } from "@/lib/format";

type Entry = Schemas["AuditEntry"];

/** The log's action names in plain words (the names themselves are in the API and the export). */
export const ACTIONS: Record<string, string> = {
  "session.sign_in": "Signed in",
  "session.failed": "Failed sign-in",
  "session.sign_out": "Signed out",
  "password.change": "Changed their password",
  "password.reset": "Reset a password",
  "user.create": "Created a user",
  "user.update": "Changed a user",
  "user.disable": "Disabled a user",
  "user.unlock": "Unlocked a user",
  "user.directory_change": "Directory changed a user",
  "user.directory_refused": "Directory refused a user",
  "token.create": "Created a token",
  "token.revoke": "Revoked a token",
  "message.payload.read": "Viewed a payload",
  "message.release": "Resent a quarantined message",
  "message.replay": "Replayed a message",
  "message.edit": "Edited and resent a message",
  "message.dismiss": "Dismissed a quarantined message",
  "destination.pause": "Paused a destination",
  "destination.resume": "Resumed a destination",
  "engine.drain": "Started a drain",
  "engine.resume": "Resumed taking messages",
  "retention.change": "Retention change started",
  "retention.confirm": "Confirmed a shorter retention",
  "doctor.run": "Ran connection checks",
  "audit.export": "Exported the audit log",
  "alert.ack": "Acknowledged an alert",
  "alert.silence": "Silenced an alert",
  "alert.unsilence": "Ended an alert's silence",
  "alert.test": "Sent a test notification",
  "engine.start": "Engine started",
  "engine.stop": "Engine stopped",
  "engine.handover": "Engine handed over",
  "engine.refused": "Engine start refused",
  "interface.stopped": "Interface stopped",
  "interface.partitions_raised": "Interface lanes raised with messages waiting",
};

/** Why a start was refused, from its detail (`reason`: `owned` or `lanes`). */
function refusal(d: Record<string, unknown>): string {
  if (d.reason === "owned") {
    const owners = Array.isArray(d.owners) ? (d.owners as Record<string, unknown>[]) : [];
    const names = Array.isArray(d.interfaces) ? (d.interfaces as string[]) : [];
    const who = owners.map((o) => `${String(o.host)} (pid ${String(o.pid)})`).join(", ");
    return `another engine runs ${names.join(", ")}${who ? ` on ${who}` : ""}`;
  }
  if (d.reason === "lanes") {
    const stranded = Array.isArray(d.stranded) ? (d.stranded as Record<string, unknown>[]) : [];
    const where = stranded.map((s) => `${String(s.interface)} → ${String(s.destination)}: ${String(s.waiting)}`);
    return `messages waiting in lanes it no longer has (${where.join(", ")})`;
  }
  if (d.reason === "handover_lanes") {
    const lowered = Array.isArray(d.lowered) ? (d.lowered as Record<string, unknown>[]) : [];
    const which = lowered.map((l) => `${String(l.interface)}: ${String(l.before)} → ${String(l.now)}`);
    return `a handover can't lower lanes (${which.join(", ")}): restart instead`;
  }
  return "";
}

/** "v0.6.0 (pid 4120) → v0.6.1 (pid 5233) · sending paused 180 ms", from an `engine.handover` detail. */
function handover(d: Record<string, unknown>): string {
  const side = (v: unknown) => {
    const s = (v ?? {}) as Record<string, unknown>;
    return `v${String(s.version ?? "")} (pid ${String(s.pid ?? "")})`;
  };
  const paused = typeof d.sending_paused_ms === "number" ? ` · sending paused ${ms(d.sending_paused_ms)}` : "";
  return `${side(d.from)} → ${side(d.to)}${paused}`;
}

const VIA: Record<string, string> = { session: "web UI", token: "API token", cli: "command line", engine: "engine" };

/** The most useful line about an entry: the reason if there is one, else what the detail says. */
export function summary(e: Entry): string {
  const d = (e.detail ?? {}) as Record<string, unknown>;
  if (e.action === "engine.refused") return refusal(d);
  if (e.action === "engine.handover") return handover(d);
  if (e.action === "interface.partitions_raised") {
    return `${String(d.before)} → ${String(d.now)} lanes with ${String(d.waiting)} waiting`;
  }
  const parts: string[] = [];
  if (typeof d.reason === "string" && d.reason) parts.push(d.reason);
  if (Array.isArray(d.messages) && d.messages.length > 1) parts.push(`${d.messages.length} messages`);
  if (typeof d.deliveries === "number" && d.deliveries > 1) parts.push(`${d.deliveries} deliveries`);
  if (Array.isArray(d.destinations) && d.destinations.length > 0) parts.push((d.destinations as string[]).join(", "));
  if (typeof d.copy === "number") parts.push(`copy ${d.copy}`);
  if (e.action === "engine.start") {
    parts.push(`v${String(d.version ?? "")}`);
    if (d.previous === "unclean") parts.push("the previous run did not stop cleanly");
  }
  if (e.action === "engine.stop") parts.push(d.forced ? "forced" : "clean");
  if (e.action === "alert.silence" && typeof d.until === "string") parts.push(`until ${timestamp(d.until)}`);
  if (e.action === "alert.ack" && typeof d.state === "string") parts.push(d.state);
  if (e.action === "interface.stopped" && typeof d.reason === "string") parts.push(d.reason);
  if (e.action === "session.failed") {
    if (typeof d.reason === "string") parts.push(d.reason);
    if (d.locked === true) parts.push("the account is now locked");
  }
  return [...new Set(parts)].join(" · ");
}

/**
 * Audit entries as a table: when, who and how, what, what it concerned, and the reason or details.
 * `whose` hides the who/how columns on a person's own activity.
 */
export function AuditTable({ entries, whose = true, label }: { entries: Entry[]; whose?: boolean; label: string }) {
  return (
    <Table aria-label={label}>
      <thead>
        <tr>
          <Th>When</Th>
          {whose && <Th>Who</Th>}
          {whose && <Th>How</Th>}
          <Th>What</Th>
          <Th>About</Th>
          <Th>Details</Th>
        </tr>
      </thead>
      <tbody>
        {entries.map((e) => (
          <tr key={e.id} className="hover:bg-hover">
            <Td className="tabular-nums">{timestamp(e.at)}</Td>
            {whose && (
              <Td>
                {e.actor}
                {e.actor_kind === "token" && <span className="text-xs text-muted"> (token)</span>}
              </Td>
            )}
            {whose && (
              <Td className="text-xs">
                {VIA[e.via ?? ""] ?? e.via ?? ""}
                {e.ip && <span className="block font-mono text-muted">{e.ip}</span>}
              </Td>
            )}
            <Td>
              {ACTIONS[e.action] ?? e.action}
              {e.outcome !== "ok" && <span className="ml-1 font-medium text-bad">({e.outcome})</span>}
            </Td>
            <Td className="text-xs break-words">
              {e.subject}
              {e.interface && e.interface !== e.subject && (
                <span className="block">
                  <Link to="/interfaces/$name" params={{ name: e.interface }}>
                    {e.interface}
                  </Link>
                </span>
              )}
              {e.message != null && (
                <Link to="/messages/$id" params={{ id: e.message }} className="block font-mono">
                  message {e.message}
                </Link>
              )}
            </Td>
            <Td className="max-w-md text-xs break-words">{summary(e)}</Td>
          </tr>
        ))}
      </tbody>
    </Table>
  );
}
