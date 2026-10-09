import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider } from "@tanstack/react-router";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { ApiError, auth } from "@/api/client";
import { makeRouter } from "@/router";
import "./index.css";

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      // A 4xx won't change by asking again; a lost connection might.
      retry: (failures, error) => !(error instanceof ApiError && error.status < 500) && failures < 2,
      refetchOnWindowFocus: true,
    },
  },
});

const router = makeRouter(queryClient);

// The session ended (expired, signed out elsewhere, token revoked): sign in again, then come back.
auth.onSignedOut = () => {
  if (router.state.location.pathname === "/sign-in") return;
  queryClient.clear();
  void router.navigate({ to: "/sign-in", search: { redirect: router.state.location.href } });
};

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>
  </StrictMode>,
);
