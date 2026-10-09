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

export const message = (id: number) =>
  queryOptions({
    queryKey: ["message", id],
    queryFn: async () => unwrap(await api.GET("/messages/{id}", { params: { path: { id } } })),
  });
