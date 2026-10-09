// What operators do: resend and correct messages, pause and resume destinations. Each call
// returns the engine's answer (or throws its refusal as an `ApiError`); `useRefresh` then
// fetches again what it changed, so every screen shows it at once.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { api, unwrap, type Schemas } from "./client";
import { session } from "./queries";

/** Operators and admins act; viewers look (the engine enforces it, this just hides the buttons). */
export function useCanOperate(): boolean {
  const { data } = useQuery(session);
  return data?.role === "operator" || data?.role === "admin";
}

/** Admins manage tokens. */
export function useIsAdmin(): boolean {
  const { data } = useQuery(session);
  return data?.role === "admin";
}

/** Fetch again everything an action can change. */
export function useRefresh() {
  const queryClient = useQueryClient();
  return () =>
    Promise.all(
      ["message", "messages", "quarantine", "interfaces", "hotspots", "retention", "drain", "engine", "tokens"].map(
        (key) => queryClient.invalidateQueries({ queryKey: [key] }),
      ),
    );
}

const reason = (text: string) => (text ? text : undefined);

/** Send a message again: to `destinations`, whatever their delivery ended as. */
export async function resend(id: number, destinations: string[], why: string) {
  return unwrap(
    await api.POST("/messages/{id}/resend", {
      params: { path: { id } },
      body: { destinations, reason: reason(why) },
    }),
  );
}

/** One quarantined delivery picked on the quarantine screen. */
export interface Picked {
  id: number;
  destination: string;
}

/**
 * Resend quarantined deliveries, grouped by destination name (at most 500 messages a call). Only
 * deliveries still quarantined go: one someone resolved meanwhile is skipped, never replayed.
 */
export async function resendQuarantined(picked: Picked[], why: string) {
  const byDestination = new Map<string, Picked[]>();
  for (const p of picked) byDestination.set(p.destination, [...(byDestination.get(p.destination) ?? []), p]);
  const result: Schemas["BulkResendResult"] = { resent: [], skipped: [] };
  for (const [destination, items] of byDestination) {
    const ids = items.map((p) => p.id);
    for (let i = 0; i < ids.length; i += 500) {
      const answer = unwrap(
        await api.POST("/messages/resend", {
          body: { ids: ids.slice(i, i + 500), destination, quarantined_only: true, reason: reason(why) },
        }),
      );
      result.resent.push(...answer.resent);
      result.skipped.push(...answer.skipped);
    }
  }
  return result;
}

/** Queue a corrected copy of a message for `destinations`. */
export async function editAndResend(id: number, payload: string, destinations: string[], why: string) {
  return unwrap(
    await api.POST("/messages/{id}/edit", {
      params: { path: { id } },
      body: { payload, destinations, reason: reason(why) },
    }),
  );
}

export async function pause(name: string, destination: string, why: string) {
  return unwrap(
    await api.POST("/interfaces/{name}/destinations/{destination}/pause", {
      params: { path: { name, destination } },
      body: { reason: reason(why) },
    }),
  );
}

export async function resume(name: string, destination: string) {
  return unwrap(
    await api.DELETE("/interfaces/{name}/destinations/{destination}/pause", {
      params: { path: { name, destination } },
    }),
  );
}

/** What moving ownership would change (writes nothing; `interfaces` empty = all of `from`'s). */
export async function previewReassign(from: string, to: string, interfaces: string[]) {
  return unwrap(await api.POST("/owners/reassign/preview", { body: { from, to, interfaces } }));
}

/** Confirm a shorter retention: deletes the interface's messages past it. */
export async function confirmRetention(name: string) {
  return unwrap(await api.POST("/retention/{name}/confirm", { params: { path: { name } } }));
}

/** Stop taking messages and deliver what's stored; with `stop`, the engine exits once drained. */
export async function startDrain(stop: boolean) {
  return unwrap(await api.POST("/drain", { body: { stop } }));
}

/** Take messages again. */
export async function endDrain() {
  return unwrap(await api.DELETE("/drain"));
}

/** Check every connection (or only what `interfaces` use) from the engine's server. */
export async function runDoctor(interfaces: string[], timeoutMs: number) {
  return unwrap(await api.POST("/doctor", { body: { interfaces, timeout_ms: timeoutMs } }));
}

export async function createToken(name: string, role: string) {
  return unwrap(await api.POST("/tokens", { body: { name, role } }));
}

export async function revokeToken(name: string) {
  const { response, error } = await api.DELETE("/tokens/{name}", { params: { path: { name } } });
  if (!response.ok) unwrap({ response, error });
}
