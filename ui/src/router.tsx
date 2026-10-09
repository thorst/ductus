import type { QueryClient } from "@tanstack/react-query";
import { createRootRouteWithContext, createRoute, createRouter, Outlet, redirect } from "@tanstack/react-router";
import { ApiError } from "@/api/client";
import { session, STATUSES, type MessageStatus } from "@/api/queries";
import { Layout } from "@/components/layout";
import { InterfacePage } from "@/pages/interface";
import { InterfacesPage } from "@/pages/interfaces";
import { MessagePage } from "@/pages/message";
import { NotFound } from "@/pages/not-found";
import { OverviewPage } from "@/pages/overview";
import { QuarantinePage } from "@/pages/quarantine";
import { SignInPage } from "@/pages/sign-in";

const root = createRootRouteWithContext<{ queryClient: QueryClient }>()({
  component: Outlet,
  notFoundComponent: NotFound,
});

const signIn = createRoute({
  getParentRoute: () => root,
  path: "/sign-in",
  validateSearch: (search: Record<string, unknown>): { redirect?: string } => ({
    redirect: typeof search.redirect === "string" && search.redirect.startsWith("/") ? search.redirect : undefined,
  }),
  component: SignInPage,
});

/** Every page but sign-in: needs a session (or goes to sign-in, and comes back after). */
const app = createRoute({
  getParentRoute: () => root,
  id: "app",
  beforeLoad: async ({ context, location }) => {
    try {
      await context.queryClient.ensureQueryData(session);
    } catch (e) {
      if (e instanceof ApiError && e.status === 401)
        throw redirect({ to: "/sign-in", search: { redirect: location.href } });
      throw e;
    }
  },
  component: Layout,
});

const overview = createRoute({ getParentRoute: () => app, path: "/", component: OverviewPage });

const interfaces = createRoute({ getParentRoute: () => app, path: "/interfaces", component: InterfacesPage });

export interface InterfaceSearch {
  tab?: "messages";
  status?: MessageStatus;
}

const iface = createRoute({
  getParentRoute: () => app,
  path: "/interfaces/$name",
  validateSearch: (search: Record<string, unknown>): InterfaceSearch => ({
    tab: search.tab === "messages" ? "messages" : undefined,
    status: STATUSES.find((s) => s === search.status),
  }),
  component: InterfacePage,
});

export interface QuarantineSearch {
  interface?: string;
  destination?: string;
}

const quarantine = createRoute({
  getParentRoute: () => app,
  path: "/quarantine",
  validateSearch: (search: Record<string, unknown>): QuarantineSearch => ({
    interface: typeof search.interface === "string" && search.interface ? search.interface : undefined,
    destination: typeof search.destination === "string" && search.destination ? search.destination : undefined,
  }),
  component: QuarantinePage,
});

const message = createRoute({
  getParentRoute: () => app,
  path: "/messages/$id",
  params: {
    parse: ({ id }) => ({ id: Number(id) }),
    stringify: ({ id }) => ({ id: String(id) }),
  },
  component: MessagePage,
});

const tree = root.addChildren([signIn, app.addChildren([overview, interfaces, iface, quarantine, message])]);

export function makeRouter(queryClient: QueryClient) {
  return createRouter({ routeTree: tree, context: { queryClient }, defaultPreload: "intent", scrollRestoration: true });
}

declare module "@tanstack/react-router" {
  interface Register {
    router: ReturnType<typeof makeRouter>;
  }
}
