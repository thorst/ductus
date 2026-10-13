# Ductus web UI

React + TypeScript + Vite, Tailwind v4, Radix primitives (shadcn/ui style, kept in `src/components/ui`), TanStack Router and Query. It uses **only the public REST API** (`/api/v1`): anything it does, a script can do too.

The engine serves it: `npm run build` writes `dist/`, which `crates/ductus-runtime/build.rs` embeds in the binary (gzipped). A binary built without `dist/` serves a page saying how to build the UI; CI sets `DUCTUS_REQUIRE_UI=1` so that can't ship.

## Working on it

```sh
npm ci
npm run dev          # http://localhost:5173, hot reload; /api goes to a running engine
                     # (DUCTUS_API_URL, default http://127.0.0.1:7700). Sign in with a token.
npm run check        # types, lint, format, unit tests (what check.sh runs)
npm run build        # dist/, and the size budget (scripts/budget.mjs: 300 KB gzipped for what
                     # index.html loads; 50 KB for each page loaded on demand)
```

- **API types** (`src/api/schema.d.ts`) are generated from the engine's OpenAPI document: `ductus api-spec > openapi.json && npm run api-types`. `scripts/dev/check.sh` does it when the API changed; CI fails if they're out of date.
- **Signing in:** a token is traded once for a session cookie (`POST /api/v1/session`) that page scripts can't read. Every request sends `X-Ductus: 1`, which the engine requires for changes made with the cookie.
- **Themes:** `src/index.css`. Every color derives from five (header, accent, page background, panel, text); light and dark are `light-dark()` pairs; high contrast meets WCAG AAA. The theme is kept in the browser until users exist (v0.5 step 8). **Custom** (`/appearance`, its own chunk) is five colors the user chooses: `src/lib/colors.ts` derives everything else as plain hex (not `color-mix()`, so the contrast check tests the numbers the browser draws), checks every pair the screens use against AA, and repairs text/accent lightness ("Fix it"); `theme.js` applies the saved values before first paint. New color pairs the UI draws belong in `check()`.
- **End-to-end:** `scripts/demo-ui.sh` starts an engine with three interfaces and runs the Playwright tests in `e2e/` against it, including axe accessibility checks in every theme (`npx playwright install --with-deps chromium` once). Two projects: `ui.spec.ts` reads as a viewer (and checks the demo's counts), then `actions.spec.ts` acts as an operator (edits, resends, pauses, confirms retention, runs the connection checks, drains) and as an admin (tokens), which changes them.
- **Actions** (`src/api/actions.ts`): buttons show for operators and admins only (the engine enforces it); each confirms in an `ActionDialog` with an optional reason, which the engine keeps with the action, and shows the engine's refusal in its own words. Tokens are admin-only (hidden from the navigation otherwise). What deletes data asks for the interface's name to be typed (shortening retention).
- **Heavy pages load on demand:** a page used rarely and costly to load goes behind `lazyRouteComponent(() => import(...))` in `router.tsx` (the API reference: 3.7 KB, its document fetched from `/api/v1/openapi.json` when opened). The budget counts only what `index.html` loads, so such a page costs a first visit nothing; `e2e/ui.spec.ts` checks the chunk isn't fetched before the page opens.
- **Owners: preview only.** Moving ownership shows the change and offers a patch and the CLI command; the engine never edits its own project (it's in git; the change is committed and deployed). Committing from the UI is planned with the Git client (v1.4).
- **Links inside sentences are underlined** (`p a` in `index.css`): axe's `link-in-text-block` (WCAG 1.4.1) fails a link told apart by color alone. A link in a list item with other text needs `className="underline"`.
- TypeScript stays on 6.0 (as the engine vendors it) until typescript-eslint supports 7.
