import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, Outlet, useNavigate } from "@tanstack/react-router";
import { Check, ChevronDown, LayoutDashboard, LogOut, Network, Radio, ShieldAlert } from "lucide-react";
import { DropdownMenu } from "radix-ui";
import { useState } from "react";
import { api } from "@/api/client";
import { useLiveUpdates, type LiveState } from "@/api/live";
import { engine, interfaces, session } from "@/api/queries";
import { Logo } from "@/components/logo";
import { count } from "@/lib/format";
import { getTheme, setTheme, THEME_LABELS, THEMES, type Theme } from "@/lib/theme";
import { cn } from "@/lib/utils";

const NAV = [
  { to: "/", label: "Overview", icon: LayoutDashboard, exact: true },
  { to: "/interfaces", label: "Interfaces", icon: Network, exact: false },
  { to: "/quarantine", label: "Quarantine", icon: ShieldAlert, exact: false },
] as const;

export function Layout() {
  const live = useLiveUpdates();
  const { data: info } = useQuery(engine);
  const draining = info && info.drain.state !== "accepting";
  return (
    <div className="flex min-h-screen flex-col">
      <a href="#main" className="sr-only focus:not-sr-only focus:absolute focus:z-50 focus:bg-panel focus:p-2">
        Skip to content
      </a>
      <header className="flex h-14 items-center gap-4 bg-header px-4 text-header-fg">
        <Link to="/" className="flex items-center gap-2 font-semibold tracking-tight text-header-fg no-underline">
          <Logo className="size-7" />
          Ductus
        </Link>
        {info && (
          <span className="hidden text-sm opacity-80 sm:inline">
            {info.host} · {info.environment} · v{info.version}
          </span>
        )}
        <div className="ml-auto flex items-center gap-3">
          {draining && (
            <span className="rounded-full bg-warn px-2.5 py-0.5 text-xs font-semibold text-black">
              {info.drain.state === "drained" ? "Drained" : "Draining"}
            </span>
          )}
          <LiveDot state={live} />
          <UserMenu />
        </div>
      </header>
      {/* Narrow screens: the same links across the top (only one of the two navs is ever shown). */}
      <nav aria-label="Main" className="border-b bg-panel px-2 py-1 md:hidden">
        <NavLinks className="flex flex-wrap gap-1" />
      </nav>
      <div className="flex flex-1">
        <nav aria-label="Main" className="hidden w-52 shrink-0 border-r bg-panel p-3 md:block">
          <NavLinks className="space-y-1" />
        </nav>
        <main id="main" className="min-w-0 flex-1 p-4 md:p-6">
          <Outlet />
        </main>
      </div>
    </div>
  );
}

function NavLinks({ className }: { className: string }) {
  const { data: all } = useQuery(interfaces);
  const quarantined = (all ?? []).flatMap((i) => i.destinations).reduce((n, d) => n + d.quarantined, 0);
  return (
    <ul className={className}>
      {NAV.map(({ to, label, icon: Icon, exact }) => (
        <li key={to}>
          <Link
            to={to}
            activeOptions={{ exact }}
            className="flex items-center gap-2 rounded-md px-3 py-2 text-sm font-medium text-text no-underline hover:bg-hover hover:no-underline data-[status=active]:bg-accent-soft data-[status=active]:text-accent"
          >
            <Icon aria-hidden className="size-4" />
            {label}
            {to === "/quarantine" && quarantined > 0 && (
              <span className="ml-auto rounded-full border border-bad/40 bg-bad/10 px-1.5 text-xs text-bad tabular-nums">
                {count(quarantined)}
              </span>
            )}
          </Link>
        </li>
      ))}
    </ul>
  );
}

const LIVE: Record<LiveState, { label: string; className: string }> = {
  live: { label: "Live", className: "text-ok" },
  connecting: { label: "Connecting", className: "text-warn" },
  offline: { label: "Offline", className: "text-bad" },
};

function LiveDot({ state }: { state: LiveState }) {
  const { label, className } = LIVE[state];
  return (
    <span className="flex items-center gap-1.5 text-xs" title="Live updates from the engine">
      <Radio aria-hidden className={cn("size-4", className)} />
      <span className="opacity-90">{label}</span>
    </span>
  );
}

function UserMenu() {
  const { data: me } = useQuery(session);
  const [theme, setCurrent] = useState<Theme>(getTheme);
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const signOut = async () => {
    await api.DELETE("/session");
    queryClient.clear();
    await navigate({ to: "/sign-in", search: {} });
  };
  return (
    <DropdownMenu.Root>
      <DropdownMenu.Trigger className="flex items-center gap-1 rounded-md px-2 py-1 text-sm hover:bg-white/10">
        <span>{me?.name}</span>
        <span className="hidden opacity-70 sm:inline">({me?.role})</span>
        <ChevronDown aria-hidden className="size-4" />
      </DropdownMenu.Trigger>
      <DropdownMenu.Portal>
        <DropdownMenu.Content
          align="end"
          sideOffset={6}
          className="z-50 min-w-52 rounded-md border bg-panel p-1 text-sm text-text shadow-lg"
        >
          <DropdownMenu.Label className="px-2 py-1.5 text-xs text-muted">Theme</DropdownMenu.Label>
          <DropdownMenu.RadioGroup
            value={theme}
            onValueChange={(value) => {
              setTheme(value as Theme);
              setCurrent(value as Theme);
            }}
          >
            {THEMES.map((t) => (
              <DropdownMenu.RadioItem
                key={t}
                value={t}
                className="flex cursor-default items-center gap-2 rounded px-2 py-1.5 outline-none data-highlighted:bg-hover"
              >
                <span className="size-4">
                  <DropdownMenu.ItemIndicator>
                    <Check aria-hidden className="size-4" />
                  </DropdownMenu.ItemIndicator>
                </span>
                {THEME_LABELS[t]}
              </DropdownMenu.RadioItem>
            ))}
          </DropdownMenu.RadioGroup>
          <DropdownMenu.Separator className="my-1 h-px bg-border" />
          <DropdownMenu.Item
            onSelect={signOut}
            className="flex cursor-default items-center gap-2 rounded px-2 py-1.5 outline-none data-highlighted:bg-hover"
          >
            <LogOut aria-hidden className="size-4" />
            Sign out
          </DropdownMenu.Item>
        </DropdownMenu.Content>
      </DropdownMenu.Portal>
    </DropdownMenu.Root>
  );
}
