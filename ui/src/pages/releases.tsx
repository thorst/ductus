import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { CircleAlert, CircleCheck, Download, RotateCcw, Upload } from "lucide-react";
import { useRef, useState } from "react";
import { pullRelease, uploadRelease, useCanDeploy, useRefresh } from "@/api/actions";
import { ApiError, type Schemas } from "@/api/client";
import { engine, releases, runs, upstream } from "@/api/queries";
import { Badge, Environments } from "@/components/badge";
import { DeployDialog } from "@/components/deploy-dialog";
import { Loaded, PageHeader } from "@/components/page";
import { Button } from "@/components/ui/button";
import { Card, CardBody, CardHeader, CardTitle } from "@/components/ui/card";
import { Table, Td, Th } from "@/components/ui/table";
import { ago, count, timestamp } from "@/lib/format";
import { rulesText } from "@/lib/releases";
import { cn } from "@/lib/utils";

/**
 * Releases (0.7.4.5): what `ductus release` made and someone stored here, where each went, how its
 * checks went, what ran in this environment and, from there, a rollback. Each release's own page
 * checks and deploys it. An engine that pulls from another (prod from test) lists that engine's
 * releases to bring here.
 */
export function ReleasesPage() {
  const info = useQuery(engine);
  const mode = info.data?.releases ?? null;
  const environment = info.data?.environment ?? "";
  return (
    <>
      <PageHeader title="Releases" actions={<UploadButton />}>
        {info.data && !mode && (
          <>
            This engine runs a project folder (folder mode): releases can be stored and checked here, and deployed to an
            engine in release mode (<code>DUCTUS_RELEASES</code>).
          </>
        )}
        {mode && <Mode mode={mode} environment={environment} />}
      </PageHeader>
      {mode?.promote_from && <Upstream environment={environment} />}
      <Stored />
      {mode && <Ran mode={mode} environment={environment} />}
    </>
  );
}

function Mode({ mode, environment }: { mode: Schemas["ReleaseMode"]; environment: string }) {
  return (
    <div className="space-y-1">
      <p>
        This engine runs releases{mode.project ? ` of ${mode.project}` : ""} in <strong>{environment}</strong>:{" "}
        {mode.running ? (
          <>
            release{" "}
            <Link to="/releases/$id" params={{ id: mode.running.slice(0, 12) }} className="font-mono">
              {mode.running.slice(0, 12)}
            </Link>{" "}
            now.
          </>
        ) : (
          "nothing is deployed yet."
        )}
      </p>
      <ul className="list-disc pl-5">
        {rulesText(mode, environment).map((r) => (
          <li key={r}>{r}</li>
        ))}
      </ul>
    </div>
  );
}

/** Store a release file from this computer (`ductus release` wrote it). */
function UploadButton() {
  const canDeploy = useCanDeploy();
  const refresh = useRefresh();
  const input = useRef<HTMLInputElement>(null);
  const [said, setSaid] = useState<{ ok: boolean; text: string } | null>(null);
  const [busy, setBusy] = useState(false);
  if (!canDeploy) return null;
  const upload = async (file: File | undefined) => {
    if (!file) return;
    setBusy(true);
    setSaid(null);
    try {
      const stored = await uploadRelease(file);
      const places = stored.new_places > 0 ? ` (${count(stored.new_places)} record(s) of where it went added)` : "";
      setSaid({
        ok: true,
        text: stored.new
          ? `Stored release ${stored.short}${places}.`
          : `Release ${stored.short} was stored here before${places}.`,
      });
      await refresh();
    } catch (err) {
      setSaid({ ok: false, text: err instanceof ApiError ? err.message : "The engine didn't answer." });
    } finally {
      setBusy(false);
      if (input.current) input.current.value = "";
    }
  };
  return (
    <div className="flex max-w-md flex-col items-end gap-2">
      <input
        ref={input}
        type="file"
        accept=".gz,.tar.gz,application/gzip"
        className="sr-only"
        aria-label="Release file"
        onChange={(e) => void upload(e.target.files?.[0])}
      />
      <Button onClick={() => input.current?.click()} disabled={busy}>
        <Upload aria-hidden />
        {busy ? "Storing…" : "Upload a release"}
      </Button>
      {said && (
        <p role={said.ok ? "status" : "alert"} className={cn("text-right text-sm", said.ok ? "text-ok" : "text-bad")}>
          {said.text}
        </p>
      )}
    </div>
  );
}

function Stored() {
  const query = useQuery(releases);
  return (
    <Card className="mb-6">
      <CardHeader>
        <CardTitle>Stored here</CardTitle>
      </CardHeader>
      <Loaded query={query}>
        {(list) =>
          list.length === 0 ? (
            <CardBody>
              <p className="text-sm text-muted">
                No release is stored on this engine&rsquo;s database yet: make one with <code>ductus release</code>,
                then upload it here or <code>ductus deploy</code> it.
              </p>
            </CardBody>
          ) : (
            <Table aria-label="Releases stored here">
              <thead>
                <tr>
                  <Th>Release</Th>
                  <Th>Made</Th>
                  <Th>Went to</Th>
                  <Th>Last checks</Th>
                </tr>
              </thead>
              <tbody>
                {list.map((r) => (
                  <tr key={r.id}>
                    <Td>
                      <Link to="/releases/$id" params={{ id: r.short }} className="font-mono">
                        {r.short}
                      </Link>
                      {r.running && <Badge className="ml-2 border-ok/40 text-ok">Running here</Badge>}
                      <div className="text-xs text-muted">
                        {r.project}
                        {r.branches.length > 0 && ` · ${r.branches.join(", ")}`}
                        {r.commit && <span className="font-mono"> · {r.commit.slice(0, 12)}</span>}
                      </div>
                    </Td>
                    <Td>
                      <div title={r.made_at ? timestamp(r.made_at) : undefined}>
                        {r.made_by}, {ago(r.made_at)}
                      </div>
                      {r.reason && <div className="text-xs text-muted">{r.reason}</div>}
                    </Td>
                    <Td>
                      <Environments names={r.environments} />
                    </Td>
                    <Td>
                      <Checks checks={r.checks} />
                    </Td>
                  </tr>
                ))}
              </tbody>
            </Table>
          )
        }
      </Loaded>
    </Card>
  );
}

function Checks({ checks }: { checks: { environment: string; ok: boolean; at: string }[] }) {
  if (checks.length === 0) return <span className="text-sm text-muted">none</span>;
  return (
    <ul className="space-y-0.5 text-sm">
      {checks.map((c) => (
        <li key={c.environment} className="flex items-center gap-1" title={timestamp(c.at)}>
          {c.ok ? (
            <CircleCheck aria-label="passed" className="size-4 text-ok" />
          ) : (
            <CircleAlert aria-label="failed" className="size-4 text-bad" />
          )}
          {c.environment}
        </li>
      ))}
    </ul>
  );
}

/** The engine this one pulls from (`DUCTUS_PROMOTE_FROM`): its releases of this project, to bring here. */
function Upstream({ environment }: { environment: string }) {
  const query = useQuery(upstream);
  const canDeploy = useCanDeploy();
  const refresh = useRefresh();
  const [busy, setBusy] = useState<string | null>(null);
  const [said, setSaid] = useState<{ ok: boolean; text: string } | null>(null);
  const bring = async (r: Schemas["UpstreamRelease"]) => {
    setBusy(r.id);
    setSaid(null);
    try {
      const pulled = await pullRelease(r.id);
      const places = pulled.new_places > 0 ? `, with ${count(pulled.new_places)} new record(s) of where it went` : "";
      setSaid({
        ok: true,
        text: `${pulled.new ? "Brought" : "Already here:"} release ${pulled.short} from ${pulled.environment}${places}. Deploy it from its page.`,
      });
      await refresh();
    } catch (err) {
      setSaid({ ok: false, text: err instanceof ApiError ? err.message : "The engine didn't answer." });
    } finally {
      setBusy(null);
    }
  };
  return (
    <Card className="mb-6">
      <CardHeader>
        <CardTitle>
          {query.data ? `On ${query.data.environment}` : "On the engine this one pulls from"}
          {query.data && <span className="ml-2 text-sm font-normal text-muted">{query.data.url}</span>}
        </CardTitle>
      </CardHeader>
      {said && (
        <CardBody>
          <p role={said.ok ? "status" : "alert"} className={cn("text-sm", said.ok ? "text-ok" : "text-bad")}>
            {said.text}
          </p>
        </CardBody>
      )}
      <Loaded query={query}>
        {(up) =>
          up.releases.length === 0 ? (
            <CardBody>
              <p className="text-sm text-muted">It has no release of this project.</p>
            </CardBody>
          ) : (
            <Table aria-label={`Releases on ${up.environment}`}>
              <thead>
                <tr>
                  <Th>Release</Th>
                  <Th>Made</Th>
                  <Th>Went to</Th>
                  <Th className="w-0" />
                </tr>
              </thead>
              <tbody>
                {up.releases.map((r) => (
                  <tr key={r.id}>
                    <Td>
                      <span className="font-mono">{r.short}</span>
                      <div className="text-xs text-muted">
                        {(r.branches ?? []).join(", ")}
                        {r.commit && <span className="font-mono"> · {r.commit.slice(0, 12)}</span>}
                      </div>
                    </Td>
                    <Td>
                      {r.made_by}, {ago(r.made_at)}
                      {r.reason && <div className="text-xs text-muted">{r.reason}</div>}
                    </Td>
                    <Td>
                      <Environments names={r.environments ?? []} />
                    </Td>
                    <Td>
                      {r.stored_here ? (
                        <Link to="/releases/$id" params={{ id: r.short }} className="text-sm whitespace-nowrap">
                          Stored here
                        </Link>
                      ) : (
                        canDeploy && (
                          <Button size="sm" variant="outline" disabled={busy !== null} onClick={() => void bring(r)}>
                            <Download aria-hidden />
                            {busy === r.id ? "Bringing…" : `Bring to ${environment}`}
                          </Button>
                        )
                      )}
                    </Td>
                  </tr>
                ))}
              </tbody>
            </Table>
          )
        }
      </Loaded>
    </Card>
  );
}

/** What ran in this environment, newest first, and a rollback to any of it. */
function Ran({ mode, environment }: { mode: Schemas["ReleaseMode"]; environment: string }) {
  const query = useQuery(runs);
  const canDeploy = useCanDeploy();
  const [back, setBack] = useState<Schemas["RunInfo"] | null>(null);
  return (
    <Card>
      <CardHeader>
        <CardTitle>What ran in {environment}</CardTitle>
      </CardHeader>
      <Loaded query={query}>
        {(list) =>
          list.length === 0 ? (
            <CardBody>
              <p className="text-sm text-muted">Nothing was deployed to {environment} yet.</p>
            </CardBody>
          ) : (
            <Table aria-label={`What ran in ${environment}`}>
              <thead>
                <tr>
                  <Th>Release</Th>
                  <Th>Deployed</Th>
                  <Th>Why</Th>
                  <Th className="w-0" />
                </tr>
              </thead>
              <tbody>
                {list.map((r) => (
                  <tr key={r.deployment}>
                    <Td>
                      <Link to="/releases/$id" params={{ id: r.short }} className="font-mono">
                        {r.short}
                      </Link>
                      {r.running && <Badge className="ml-2 border-ok/40 text-ok">Running</Badge>}
                      {r.rollback && <Badge className="ml-2">Rollback</Badge>}
                    </Td>
                    <Td>
                      <span title={timestamp(r.at)}>
                        {ago(r.at)} by {r.by}
                      </span>
                    </Td>
                    <Td className="text-sm">{r.reason ?? <span className="text-muted">–</span>}</Td>
                    <Td>
                      {canDeploy && r.release !== mode.running && (
                        <Button
                          size="sm"
                          variant={r.rollback_target ? "primary" : "outline"}
                          onClick={() => setBack(r)}
                        >
                          <RotateCcw aria-hidden />
                          Roll back to it…
                        </Button>
                      )}
                    </Td>
                  </tr>
                ))}
              </tbody>
            </Table>
          )
        }
      </Loaded>
      {back && (
        <DeployDialog
          open
          onOpenChange={(o) => !o && setBack(null)}
          release={back.release}
          short={back.short}
          rollback
          environment={environment}
          mode={mode}
        />
      )}
    </Card>
  );
}
