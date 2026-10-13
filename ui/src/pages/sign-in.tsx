import { useQueryClient } from "@tanstack/react-query";
import { getRouteApi, useNavigate } from "@tanstack/react-router";
import { KeyRound } from "lucide-react";
import { useState, type FormEvent } from "react";
import { api, unwrap, ApiError } from "@/api/client";
import { session } from "@/api/queries";
import { Logo } from "@/components/logo";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";

const route = getRouteApi("/sign-in");

/**
 * Sign in with a username and password (people) or an API token (scripts, and the first minutes of
 * a new install). The engine answers with a session cookie; neither the password nor the token
 * is kept in the browser.
 */
export function SignInPage() {
  const { redirect } = route.useSearch();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const [withToken, setWithToken] = useState(false);
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [token, setToken] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const ready = withToken ? token.trim() !== "" : username.trim() !== "" && password !== "";

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const body = withToken ? { token: token.trim() } : { username: username.trim(), password };
      const me = unwrap(await api.POST("/session", { body }));
      // The browser may have refused the cookie (plain http to another machine): check.
      const check = await api.GET("/session");
      if (!check.response.ok) {
        setError(
          "Signed in, but this browser didn't keep the session cookie. Open the UI over https, " +
            "or at http://localhost:7700 on the engine's own server.",
        );
        return;
      }
      queryClient.setQueryData(session.queryKey, check.data);
      // A password an admin chose has to be changed before anything else works.
      await navigate({ to: me.must_change_password ? "/account" : (redirect ?? "/") });
    } catch (err) {
      setPassword("");
      setError(err instanceof ApiError ? err.message : "The engine didn't answer. Is it running?");
    } finally {
      setBusy(false);
    }
  };

  return (
    <main className="flex min-h-screen items-center justify-center p-4">
      <form onSubmit={submit} className="w-full max-w-sm space-y-5 rounded-lg border bg-panel p-6 shadow-sm">
        <div className="flex items-center gap-3">
          <Logo className="size-9" />
          <div>
            <h1 className="text-lg font-semibold">Sign in to Ductus</h1>
            <p className="text-sm text-muted">{withToken ? "with an API token" : "with your username and password"}</p>
          </div>
        </div>
        {withToken ? (
          <label className="block space-y-1.5">
            <span className="text-sm font-medium">Token</span>
            <Input
              type="password"
              autoComplete="off"
              autoFocus
              required
              placeholder="dux_…"
              value={token}
              onChange={(e) => setToken(e.target.value)}
              aria-describedby={error ? "sign-in-error" : "sign-in-help"}
            />
          </label>
        ) : (
          <>
            <label className="block space-y-1.5">
              <span className="text-sm font-medium">Username</span>
              <Input
                autoComplete="username"
                autoFocus
                required
                value={username}
                onChange={(e) => setUsername(e.target.value)}
              />
            </label>
            <label className="block space-y-1.5">
              <span className="text-sm font-medium">Password</span>
              <Input
                type="password"
                autoComplete="current-password"
                required
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                aria-describedby={error ? "sign-in-error" : "sign-in-help"}
              />
            </label>
          </>
        )}
        {error && (
          <p id="sign-in-error" role="alert" className="rounded-md border border-bad/40 bg-bad/10 p-3 text-sm text-bad">
            {error}
          </p>
        )}
        <Button type="submit" className="w-full" disabled={busy || !ready}>
          <KeyRound aria-hidden />
          {busy ? "Signing in…" : "Sign in"}
        </Button>
        <p id="sign-in-help" className="text-xs text-muted">
          {withToken ? (
            <>
              Make one on the server: <code className="font-mono">ductus token create your-name --role viewer</code>
              .{" "}
            </>
          ) : (
            <>
              No account yet? On the server:{" "}
              <code className="font-mono">ductus user create your-name --role admin</code>.{" "}
            </>
          )}
          The browser keeps a session (12 hours idle, 7 days at most), not the {withToken ? "token" : "password"}.
        </p>
        <button
          type="button"
          className="text-sm text-accent underline"
          onClick={() => {
            setWithToken(!withToken);
            setError(null);
          }}
        >
          {withToken ? "Use a username and password instead" : "Use an API token instead"}
        </button>
      </form>
    </main>
  );
}
