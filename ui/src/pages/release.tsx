import { useQuery } from "@tanstack/react-query";
import { getRouteApi, Link } from "@tanstack/react-router";
import { Download, Rocket, ShieldCheck } from "lucide-react";
import { useState } from "react";
import { checkRelease, useCanDeploy, useRefresh } from "@/api/actions";
import { ApiError, type Schemas } from "@/api/client";
import { engine, release, releaseManifest } from "@/api/queries";
import { Badge, Environments } from "@/components/badge";
import { DeployDialog } from "@/components/deploy-dialog";
import { CheckVerdict, GateReport } from "@/components/gate-report";
import { Field, Loaded, PageHeader } from "@/components/page";
import { Button } from "@/components/ui/button";
import { Card, CardBody, CardHeader, CardTitle } from "@/components/ui/card";
import { Table, Td, Th } from "@/components/ui/table";
import { bytes, count, timestamp } from "@/lib/format";
import { changesSummary } from "@/lib/releases";

/**
 * One release: what it is, what deploying it here would change, where it went, every check of
 * it with its report; and, for those with `deploy`, Check against this environment, Deploy here
 * and Download (the file with its record of where it went).
 */
const route = getRouteApi("/app/releases/$id");

export function ReleasePage() {
  const { id } = route.useParams();
  const query = useQuery(release(id));
  return <Loaded query={query}>{(r) => <Release release={r} />}</Loaded>;
}

function Release({ release: r }: { release: Schemas["ReleaseDetail"] }) {
  const info = useQuery(engine);
  const canDeploy = useCanDeploy();
  const refresh = useRefresh();
  const [deploying, setDeploying] = useState(false);
  const [checking, setChecking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const mode = info.data?.releases ?? null;
  const environment = info.data?.environment ?? "";
  const check = async () => {
    setChecking(true);
    setError(null);
    try {
      await checkRelease(r.id);
      await refresh();
    } catch (err) {
      setError(err instanceof ApiError ? err.message : "The engine didn't answer.");
    } finally {
      setChecking(false);
    }
  };
  const ofThisProject = !mode?.project || mode.project === r.project;
  return (
    <>
      <PageHeader
        title={
          <>
            Release <span className="font-mono">{r.short}</span>
            {r.running && <Badge className="ml-3 align-middle border-ok/40 text-ok">Running here</Badge>}
          </>
        }
        actions={
          canDeploy && (
            <div className="flex flex-wrap gap-2">
              <Button variant="outline" disabled={checking} onClick={() => void check()}>
                <ShieldCheck aria-hidden />
                {checking ? "Checking…" : `Check against ${environment}`}
              </Button>
              <Button variant="outline" asChild>
                <a href={`/api/v1/releases/${r.id}/file`} download>
                  <Download aria-hidden />
                  Download
                </a>
              </Button>
              {mode && !r.running && ofThisProject && (
                <Button onClick={() => setDeploying(true)}>
                  <Rocket aria-hidden />
                  Deploy to {environment}…
                </Button>
              )}
            </div>
          )
        }
      >
        <Link to="/releases">Releases</Link> · {r.project}
        {r.branches.length > 0 && ` · made on ${r.branches.join(", ")}`}
        {r.commit && <span className="font-mono"> · {r.commit.slice(0, 12)}</span>}
      </PageHeader>
      {error && (
        <p role="alert" className="mb-4 rounded-md border border-bad/40 bg-bad/10 p-3 text-sm text-bad">
          {error}
        </p>
      )}
      {mode && !ofThisProject && (
        <p className="mb-4 text-sm text-warn">
          This engine runs {mode.project}; this release is of {r.project}: deploy it to that project&rsquo;s engine.
        </p>
      )}
      <Card className="mb-6">
        <CardBody>
          <dl className="grid gap-4 sm:grid-cols-2 lg:grid-cols-4">
            <Field label="Made">
              {r.made_by}, {timestamp(r.made_at)}
              {r.reason && <div className="text-muted">{r.reason}</div>}
            </Field>
            <Field label="Stored here">
              {r.stored_by}, {timestamp(r.stored_at)}
            </Field>
            <Field label="Holds">
              {count(r.interfaces)} interface{r.interfaces === 1 ? "" : "s"}, {count(r.files)} files, {bytes(r.bytes)}
            </Field>
            <Field label="Went to">
              <Environments names={r.environments} />
            </Field>
          </dl>
        </CardBody>
      </Card>
      {r.here && <Here here={r.here} />}
      <Went places={r.deployed} />
      <Checks checks={r.checks} />
      <Interfaces id={r.id} />
      {deploying && mode && (
        <DeployDialog
          open
          onOpenChange={setDeploying}
          release={r.id}
          short={r.short}
          environment={environment}
          mode={mode}
          here={r.here}
        />
      )}
    </>
  );
}

const CHANGE: Record<string, string> = { changed: "changes", new: "new here", removed: "removed" };

function Here({ here }: { here: Schemas["Here"] }) {
  return (
    <Card className="mb-6">
      <CardHeader>
        <CardTitle>What it changes in {here.environment}</CardTitle>
      </CardHeader>
      <CardBody>
        <p className="text-sm">{changesSummary(here)}</p>
        {here.ran_before && !here.running && (
          <p className="mt-1 text-sm text-muted">It ran here before: a rollback can go back to it.</p>
        )}
      </CardBody>
      {here.changes.length > 0 && (
        <Table aria-label={`What it changes in ${here.environment}`}>
          <thead>
            <tr>
              <Th>Interface</Th>
              <Th>Change</Th>
              <Th>Code version</Th>
            </tr>
          </thead>
          <tbody>
            {here.changes.map((c) => (
              <tr key={c.interface}>
                <Td className="font-medium">
                  {c.change === "new" ? (
                    c.interface
                  ) : (
                    <Link to="/interfaces/$name" params={{ name: c.interface }}>
                      {c.interface}
                    </Link>
                  )}
                </Td>
                <Td>{CHANGE[c.change] ?? c.change}</Td>
                <Td className="font-mono text-xs">
                  {c.from ?? "–"} → {c.to ?? "–"}
                </Td>
              </tr>
            ))}
          </tbody>
        </Table>
      )}
    </Card>
  );
}

function Went({ places }: { places: Schemas["PlaceInfo"][] }) {
  return (
    <Card className="mb-6">
      <CardHeader>
        <CardTitle>Where it went</CardTitle>
      </CardHeader>
      {places.length === 0 ? (
        <CardBody>
          <p className="text-sm text-muted">Deployed nowhere this engine knows of.</p>
        </CardBody>
      ) : (
        <Table aria-label="Where it went">
          <thead>
            <tr>
              <Th>Environment</Th>
              <Th>When</Th>
              <Th>Engine</Th>
              <Th>Known from</Th>
            </tr>
          </thead>
          <tbody>
            {places.map((p) => (
              <tr key={`${p.environment}-${p.host}-${p.deployment}`}>
                <Td className="font-medium">{p.environment}</Td>
                <Td>
                  {timestamp(p.at)} by {p.by}
                </Td>
                <Td className="text-sm">
                  {p.host}, Ductus {p.ductus}
                </Td>
                <Td className="text-sm">
                  {p.here ? "this database" : <span title="Unsigned: the file said so">the release&rsquo;s file</span>}
                </Td>
              </tr>
            ))}
          </tbody>
        </Table>
      )}
    </Card>
  );
}

function Checks({ checks }: { checks: Schemas["CheckInfo"][] }) {
  return (
    <Card className="mb-6">
      <CardHeader>
        <CardTitle>Checks</CardTitle>
      </CardHeader>
      <CardBody>
        {checks.length === 0 ? (
          <p className="text-sm text-muted">Never checked here.</p>
        ) : (
          <ul className="space-y-3">
            {checks.map((c, n) => (
              <li key={c.id}>
                <details open={n === 0}>
                  <summary className="cursor-pointer">
                    <CheckVerdict check={c} />
                  </summary>
                  <div className="mt-2 ml-5">
                    <GateReport check={c} />
                  </div>
                </details>
              </li>
            ))}
          </ul>
        )}
      </CardBody>
    </Card>
  );
}

interface ManifestInterface {
  version?: string;
  files?: string[];
}

/** Its interfaces with their code versions and files: fetched only when asked (the manifest is large). */
function Interfaces({ id }: { id: string }) {
  const [open, setOpen] = useState(false);
  const query = useQuery({ ...releaseManifest(id), enabled: open });
  const list = Object.entries(
    ((query.data as { interfaces?: Record<string, ManifestInterface> } | null)?.interfaces ?? {}) as Record<
      string,
      ManifestInterface
    >,
  );
  return (
    <Card>
      <CardHeader className="flex items-center justify-between">
        <CardTitle>Its interfaces</CardTitle>
        {!open && (
          <Button variant="outline" size="sm" onClick={() => setOpen(true)}>
            Show them
          </Button>
        )}
      </CardHeader>
      {open && (
        <Loaded query={query}>
          {() => (
            <Table aria-label="Its interfaces">
              <thead>
                <tr>
                  <Th>Interface</Th>
                  <Th>Code version</Th>
                  <Th>Files</Th>
                </tr>
              </thead>
              <tbody>
                {list.map(([name, i]) => (
                  <tr key={name}>
                    <Td className="font-medium">{name}</Td>
                    <Td className="font-mono text-xs">{(i.version ?? "").slice(0, 12)}</Td>
                    <Td className="text-xs">{(i.files ?? []).join(", ")}</Td>
                  </tr>
                ))}
              </tbody>
            </Table>
          )}
        </Loaded>
      )}
    </Card>
  );
}
