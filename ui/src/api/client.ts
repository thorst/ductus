// The REST API, typed from its OpenAPI document (schema.d.ts is generated: `npm run api-types`
// after `ductus api-spec > openapi.json`). The UI uses only this public API.
//
// The browser is signed in by a session cookie the engine set (scripts can't read it). Every
// request carries `X-Ductus: 1`, which the engine requires for changes made with the cookie:
// other sites can't send it, so they can't act through someone's session.

import createClient, { type Middleware } from "openapi-fetch";
import type { components, paths } from "./schema";

export type Schemas = components["schemas"];

export const api = createClient<paths>({ baseUrl: "/api/v1", credentials: "same-origin" });

/** An answer that isn't 2xx: the engine's own words (`{"error": "..."}`) and the status. */
export class ApiError extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message);
  }
}

/** Set by the app: what to do when the session has ended (go to the sign-in page). */
export const auth = { onSignedOut: () => {} };

const middleware: Middleware = {
  onRequest({ request }) {
    request.headers.set("X-Ductus", "1");
    return request;
  },
  onResponse({ response, request }) {
    if (response.status === 401 && !new URL(request.url).pathname.endsWith("/session")) auth.onSignedOut();
    return response;
  },
};
api.use(middleware);

/** The data of an openapi-fetch result, or its error thrown as an `ApiError`. */
export function unwrap<T>(result: { data?: T; error?: unknown; response: Response }): T {
  if (result.response.ok && result.data !== undefined) return result.data;
  const error = result.error as { error?: string } | undefined;
  throw new ApiError(result.response.status, error?.error ?? `${result.response.status} ${result.response.statusText}`);
}
