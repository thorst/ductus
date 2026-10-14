// What each screen reads, as TanStack Query options (cached, shared, refreshed live).

import { infiniteQueryOptions, queryOptions } from "@tanstack/react-query";
import { api, unwrap } from "./client";

/** Health words change with time even when no count does ("silent for 47 min"). */
const HEALTH_REFRESH_MS = 15_000;

export const session = queryOptions({
  queryKey: ["session"],
  queryFn: async () => unwrap(await api.GET("/session")),
  staleTime: 60_000,
  retry: false,
});

export const engine = queryOptions({
  queryKey: ["engine"],
  queryFn: async () => unwrap(await api.GET("/engine")),
  refetchInterval: HEALTH_REFRESH_MS,
});

export const interfaces = queryOptions({
  queryKey: ["interfaces"],
  queryFn: async () => unwrap(await api.GET("/interfaces")),
  refetchInterval: HEALTH_REFRESH_MS,
});

export const iface = (name: string) =>
  queryOptions({
    queryKey: ["interfaces", name],
    queryFn: async () => unwrap(await api.GET("/interfaces/{name}", { params: { path: { name } } })),
    refetchInterval: HEALTH_REFRESH_MS,
  });

export const STATUSES = ["pending", "sent", "filtered", "quarantined", "duplicate"] as const;
export type MessageStatus = (typeof STATUSES)[number];

export const PAGE = 100;

/** An interface's messages, newest first, a page at a time (older pages as you scroll). */
export const messages = (name: string, status?: MessageStatus) =>
  infiniteQueryOptions({
    queryKey: ["messages", name, status ?? "all"],
    queryFn: async ({ pageParam }) =>
      unwrap(
        await api.GET("/interfaces/{name}/messages", {
          params: { path: { name }, query: { status, before: pageParam ?? undefined, limit: PAGE } },
        }),
      ),
    initialPageParam: null as number | null,
    getNextPageParam: (page) => page.next_before ?? null,
  });

/** Quarantined deliveries across interfaces (or one interface or destination), newest first. */
export const quarantine = (filter: { interface?: string; destination?: string }) =>
  infiniteQueryOptions({
    queryKey: ["quarantine", filter.interface ?? "", filter.destination ?? ""],
    queryFn: async ({ pageParam }) =>
      unwrap(
        await api.GET("/quarantine", {
          params: { query: { ...filter, before: pageParam ?? undefined, limit: PAGE } },
        }),
      ),
    initialPageParam: null as string | null,
    getNextPageParam: (page) => page.next_before ?? null,
  });

export const message = (id: number) =>
  queryOptions({
    queryKey: ["message", id],
    queryFn: async () => unwrap(await api.GET("/messages/{id}", { params: { path: { id } } })),
  });

/** Where each backlog sits (on demand: it counts every lane's queue). */
export const hotspots = (name?: string, lanes = 20) =>
  queryOptions({
    queryKey: ["hotspots", name ?? "", lanes],
    queryFn: async () => unwrap(await api.GET("/hotspots", { params: { query: { interface: name, lanes } } })),
    refetchInterval: HEALTH_REFRESH_MS,
  });

export type ImpactSearch = { vendor?: string; application?: string; host?: string; resolve?: boolean };

/** Which interfaces a vendor, application or host touches (exactly one of the three). */
export const impact = (search: ImpactSearch) =>
  queryOptions({
    queryKey: ["impact", search],
    queryFn: async () => unwrap(await api.GET("/impact", { params: { query: search } })),
    enabled: Boolean(search.vendor || search.application || search.host),
    staleTime: 60_000,
  });

export const owners = queryOptions({
  queryKey: ["owners"],
  queryFn: async () => unwrap(await api.GET("/owners")),
});

export const retention = queryOptions({
  queryKey: ["retention"],
  queryFn: async () => unwrap(await api.GET("/retention")),
  // A change in progress counts down its rows.
  refetchInterval: HEALTH_REFRESH_MS,
});

/** Draining is followed closely: it's what someone waits on during an upgrade. */
export const drain = queryOptions({
  queryKey: ["drain"],
  queryFn: async () => unwrap(await api.GET("/drain")),
  refetchInterval: (query) => (query.state.data?.state === "draining" ? 2_000 : HEALTH_REFRESH_MS),
});

export const tokens = queryOptions({
  queryKey: ["tokens"],
  queryFn: async () => unwrap(await api.GET("/tokens")),
});

export const users = queryOptions({
  queryKey: ["users"],
  queryFn: async () => unwrap(await api.GET("/users")),
});

/** The signed-in person's own account (people only: a token has none). */
export const account = queryOptions({
  queryKey: ["me", "account"],
  queryFn: async () => unwrap(await api.GET("/me")),
});

export const mySessions = queryOptions({
  queryKey: ["me", "sessions"],
  queryFn: async () => unwrap(await api.GET("/me/sessions")),
});

export const myTokens = queryOptions({
  queryKey: ["me", "tokens"],
  queryFn: async () => unwrap(await api.GET("/me/tokens")),
});

/** `from` and `to` are times (ISO 8601): the page turns the days typed in the filter into them. */
export interface AuditSearch {
  actor?: string;
  action?: string;
  interface?: string;
  message?: number;
  from?: string;
  to?: string;
}

/** The audit log, newest first, a page at a time (needs the `audit` permission). */
export const audit = (filter: AuditSearch) =>
  infiniteQueryOptions({
    queryKey: ["audit", filter],
    queryFn: async ({ pageParam }) =>
      unwrap(
        await api.GET("/audit", { params: { query: { ...filter, before: pageParam ?? undefined, limit: PAGE } } }),
      ),
    initialPageParam: null as number | null,
    getNextPageParam: (page) => page.next_before ?? null,
  });

/** Who looked at a message's payload (for those who may read the audit log). */
export const messageReaders = (id: number) =>
  queryOptions({
    queryKey: ["audit", "readers", id],
    queryFn: async () =>
      unwrap(
        await api.GET("/audit", { params: { query: { message: id, action: "message.payload.read", limit: 50 } } }),
      ),
  });

/** What the signed-in person did themselves. */
export const myActivity = queryOptions({
  queryKey: ["me", "activity"],
  queryFn: async () => unwrap(await api.GET("/me/activity", { params: { query: { limit: 25 } } })),
});

/** The company-defined documentation fields and every entry's values (the project's files, so rarely changing). */
export const docs = queryOptions({
  queryKey: ["docs"],
  queryFn: async () => unwrap(await api.GET("/docs")),
  staleTime: 60_000,
});

/** Alerts: every interface's and destination's, open ones first, and what happened lately. */
export const alerts = queryOptions({
  queryKey: ["alerts"],
  queryFn: async () => unwrap(await api.GET("/alerts")),
  refetchInterval: HEALTH_REFRESH_MS,
});

/** Where alerts go and how sending went lately (operators: it shows addresses). */
export const alertChannels = queryOptions({
  queryKey: ["alerts", "channels"],
  queryFn: async () => unwrap(await api.GET("/alerts/channels")),
  refetchInterval: HEALTH_REFRESH_MS,
});
