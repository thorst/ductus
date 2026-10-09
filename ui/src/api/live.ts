// Live updates: the engine's `/events` stream (Server-Sent Events) says when queues, a health
// state or the drain state change; the screens' data is then fetched again. The browser sends
// the session cookie with it (same origin) and reconnects by itself after a network blip.

import { useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";

export type LiveState = "connecting" | "live" | "offline";

export function useLiveUpdates(): LiveState {
  const queryClient = useQueryClient();
  const [state, setState] = useState<LiveState>("connecting");
  useEffect(() => {
    const events = new EventSource("/api/v1/events");
    events.onopen = () => setState("live");
    events.onerror = () => setState(events.readyState === EventSource.CLOSED ? "offline" : "connecting");
    events.addEventListener("update", () => {
      setState("live");
      void queryClient.invalidateQueries({ queryKey: ["interfaces"] });
      void queryClient.invalidateQueries({ queryKey: ["engine"] });
      void queryClient.invalidateQueries({ queryKey: ["quarantine"] });
      void queryClient.invalidateQueries({ queryKey: ["message"] }); // an open message's statuses
    });
    return () => events.close();
  }, [queryClient]);
  return state;
}
