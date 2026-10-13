import type { QueryClient } from "@tanstack/react-query";
import {
  createRootRouteWithContext,
  createRoute,
  createRouter,
  lazyRouteComponent,
  Outlet,
  redirect,
} from "@tanstack/react-router";
import { ApiError } from "@/api/client";
import { session, STATUSES, type ImpactSearch, type MessageStatus } from "@/api/queries";
import { Layout } from "@/components/layout";
import { DoctorPage } from "@/pages/doctor";
import { DrainPage } from "@/pages/drain";
import { HotspotsPage } from "@/pages/hotspots";
import { ImpactPage } from "@/pages/impact";
import { InterfacePage } from "@/pages/interface";
import { InterfacesPage } from "@/pages/interfaces";
import { MessagePage } from "@/pages/message";
import { NotFound } from "@/pages/not-found";
import { OverviewPage } from "@/pages/overview";
import { OwnersPage } from "@/pages/owners";
import { QuarantinePage } from "@/pages/quarantine";
import { RetentionPage } from "@/pages/retention";
import { SignInPage } from "@/pages/sign-in";
import { TokensPage } from "@/pages/tokens";

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

const str = (v: unknown) => (typeof v === "string" && v ? v : undefined);

export interface HotspotsSearch {
  interface?: string;
  destination?: string;
}

const hotspots = createRoute({
  getParentRoute: () => app,
  path: "/hotspots",
  validateSearch: (search: Record<string, unknown>): HotspotsSearch => ({
    interface: str(search.interface),
    destination: str(search.destination),
  }),
  component: HotspotsPage,
});

const impact = createRoute({
  getParentRoute: () => app,
  path: "/impact",
  validateSearch: (search: Record<string, unknown>): ImpactSearch => ({
    vendor: str(search.vendor),
    application: str(search.application),
    host: str(search.host),
    resolve: search.resolve === true || search.resolve === "true" ? true : undefined,
  }),
  component: ImpactPage,
});

const owners = createRoute({ getParentRoute: () => app, path: "/owners", component: OwnersPage });
const retention = createRoute({ getParentRoute: () => app, path: "/retention", component: RetentionPage });
const drain = createRoute({ getParentRoute: () => app, path: "/drain", component: DrainPage });
const doctor = createRoute({ getParentRoute: () => app, path: "/connections", component: DoctorPage });
const tokens = createRoute({ getParentRoute: () => app, path: "/tokens", component: TokensPage });

/** Its own chunk, fetched when opened: not part of a first visit's size (scripts/budget.mjs). */
const apiReference = createRoute({
  getParentRoute: () => app,
  path: "/api-reference",
  component: lazyRouteComponent(() => import("@/pages/api-reference"), "ApiReferencePage"),
});

/** Also on demand: it carries the color math (src/lib/colors.ts). */
const appearance = createRoute({
  getParentRoute: () => app,
  path: "/appearance",
  component: lazyRouteComponent(() => import("@/pages/appearance"), "AppearancePage"),
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

const tree = root.addChildren([
  signIn,
  app.addChildren([
    overview,
    interfaces,
    iface,
    quarantine,
    hotspots,
    impact,
    owners,
    retention,
    drain,
    doctor,
    tokens,
    apiReference,
    appearance,
    message,
  ]),
]);

export function makeRouter(queryClient: QueryClient) {
  return createRouter({ routeTree: tree, context: { queryClient }, defaultPreload: "intent", scrollRestoration: true });
}

declare module "@tanstack/react-router" {
  interface Register {
    router: ReturnType<typeof makeRouter>;
  }
}
