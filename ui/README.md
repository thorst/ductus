# Ductus web UI

React + TypeScript + Vite, Tailwind v4, Radix primitives (shadcn/ui style, kept in `src/components/ui`), TanStack Router and Query. It uses **only the public REST API** (`/api/v1`): anything it does, a script can do too.

The engine serves it: `npm run build` writes `dist/`, which `crates/ductus-runtime/build.rs` embeds in the binary (gzipped). A binary built without `dist/` serves a page saying how to build the UI; CI sets `DUCTUS_REQUIRE_UI=1` so that can't ship.

## Working on it

```sh
npm ci
npm run dev          # http://localhost:5173, hot reload; /api goes to a running engine
                     # (DUCTUS_API_URL, default http://127.0.0.1:7700). Sign in with a token.
npm run check        # types, lint, format, unit tests (what check.sh runs)
npm run build        # dist/, and the size budget (scripts/budget.mjs: 300 KB gzipped)
```

- **API types** (`src/api/schema.d.ts`) are generated from the engine's OpenAPI document: `ductus api-spec > openapi.json && npm run api-types`. `scripts/dev/check.sh` does it when the API changed; CI fails if they're out of date.
- **Signing in:** a token is traded once for a session cookie (`POST /api/v1/session`) that page scripts can't read. Every request sends `X-Ductus: 1`, which the engine requires for changes made with the cookie.
- **Themes:** `src/index.css`. Every color derives from five (header, accent, page background, panel, text); light and dark are `light-dark()` pairs; high contrast meets WCAG AAA. The theme is kept in the browser until users exist (v0.5 step 8).
- **End-to-end:** `scripts/demo-ui.sh` starts an engine with three interfaces and runs the Playwright tests in `e2e/` against it, including axe accessibility checks in every theme (`npx playwright install --with-deps chromium` once).
- TypeScript stays on 6.0 (as the engine vendors it) until typescript-eslint supports 7.
