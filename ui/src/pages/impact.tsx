import { useQuery } from "@tanstack/react-query";
import { getRouteApi, Link, useNavigate } from "@tanstack/react-router";
import { CircleCheck, Search } from "lucide-react";
import { useState, type FormEvent } from "react";
import type { Schemas } from "@/api/client";
import { impact, type ImpactSearch } from "@/api/queries";
import { Loaded, PageHeader } from "@/components/page";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Table, Td, Th } from "@/components/ui/table";
import { count } from "@/lib/format";

const route = getRouteApi("/app/impact");

type Kind = "vendor" | "application" | "host";

const KINDS: Record<Kind, { label: string; placeholder: string }> = {
  vendor: { label: "Vendor", placeholder: "Epic Systems" },
  application: { label: "Application", placeholder: "Sunquest Lab" },
  host: { label: "Host or subnet", placeholder: "10.20.30.0/24 or lab.example.org" },
};

const kindOf = (s: ImpactSearch): Kind => (s.application ? "application" : s.host ? "host" : "vendor");

/**
 * Which interfaces a vendor, application or host touches, how, and who owns them: for a vendor
 * breach or a VPN or IP change. The same report as `ductus impact`.
 */
export function ImpactPage() {
  const search = route.useSearch();
  const navigate = useNavigate({ from: "/impact" });
  const [kind, setKind] = useState<Kind>(kindOf(search));
  const [value, setValue] = useState(search[kindOf(search)] ?? "");
  const [resolve, setResolve] = useState(search.resolve ?? false);
  const query = useQuery(impact(search));
  const asked = Boolean(search.vendor || search.application || search.host);

  const submit = (e: FormEvent) => {
    e.preventDefault();
    const v = value.trim();
    if (v) void navigate({ search: { [kind]: v, resolve: kind === "host" && resolve ? true : undefined } });
  };
  const lookFor = (name: string) => {
    setValue(name);
    void navigate({ search: { [kind]: name } });
  };

  return (
    <>
      <PageHeader title="Impact">
        Which interfaces a vendor, an application or a host touches, how, and who owns them.
      </PageHeader>
      <form onSubmit={submit} role="search" className="mb-6 flex flex-wrap items-end gap-3">
        <label className="text-sm">
          <span className="block text-xs text-muted">Look for</span>
          <select
            className="mt-1 h-9 rounded-md border bg-panel px-2 text-sm"
            value={kind}
            onChange={(e) => setKind(e.target.value as Kind)}
          >
            {Object.entries(KINDS).map(([k, { label }]) => (
              <option key={k} value={k}>
                {label}
              </option>
            ))}
          </select>
        </label>
        <label className="min-w-64 flex-1 text-sm">
          <span className="block text-xs text-muted">{KINDS[kind].label}</span>
          <Input
            className="mt-1"
            value={value}
            placeholder={KINDS[kind].placeholder}
            onChange={(e) => setValue(e.target.value)}
          />
        </label>
        {kind === "host" && (
          <label className="flex h-9 items-center gap-2 text-sm">
            <input type="checkbox" checked={resolve} onChange={(e) => setResolve(e.target.checked)} />
            Look names up in DNS (from the engine's server)
          </label>
        )}
        <Button type="submit">
          <Search aria-hidden />
          Find
        </Button>
      </form>
      {asked && <Loaded query={query}>{(report) => <Report report={report} onKnown={lookFor} />}</Loaded>}
    </>
  );
}

function Report({ report, onKnown }: { report: Schemas["ImpactReport"]; onKnown: (name: string) => void }) {
  return (
    <div className="space-y-4">
      <p role="status" className="text-sm">
        {report.affected === 0 ? (
          <span className="inline-flex items-center gap-2 text-ok">
            <CircleCheck aria-hidden className="size-5" />
            No interface touches {report.query} (of {count(report.interfaces)}).
          </span>
        ) : (
          <>
            <strong>
              {count(report.affected)} of {count(report.interfaces)}
            </strong>{" "}
            {report.interfaces === 1 ? "interface touches" : "interfaces touch"} {report.query}.
          </>
        )}
      </p>
      {report.known.length > 0 && (
        <div className="text-sm">
          <span className="text-muted">Known names: </span>
          <ul className="inline">
            {report.known.map((k) => (
              <li key={k} className="mr-2 inline-block">
                <Button size="sm" variant="outline" onClick={() => onKnown(k)}>
                  {k}
                </Button>
              </li>
            ))}
          </ul>
        </div>
      )}
      {report.hits.length > 0 && (
        <Card>
          <Table aria-label="Affected interfaces">
            <thead>
              <tr>
                <Th>Interface</Th>
                <Th>Owner</Th>
                <Th>How</Th>
                <Th>Why</Th>
                <Th>Hosts, ports, folders</Th>
              </tr>
            </thead>
            <tbody>
              {report.hits.map((h, n) => (
                <tr key={n}>
                  <Td>
                    <Link to="/interfaces/$name" params={{ name: h.interface }} className="font-medium">
                      {h.interface}
                    </Link>
                  </Td>
                  <Td>{h.owner ?? <span className="text-muted">nobody</span>}</Td>
                  <Td>{h.role}</Td>
                  <Td className="text-xs">{h.reason}</Td>
                  <Td className="font-mono text-xs">
                    {h.endpoints.map((e) => (
                      <div key={e}>{e}</div>
                    ))}
                  </Td>
                </tr>
              ))}
            </tbody>
          </Table>
        </Card>
      )}
      {report.incomplete.length > 0 && (
        <p className="rounded-md border border-warn/40 bg-warn/10 p-3 text-sm">
          <strong>May be missing here:</strong> {report.incomplete.join(", ")}{" "}
          {report.incomplete.length === 1 ? "doesn't" : "don't"} name every system{" "}
          {report.incomplete.length === 1 ? "it connects" : "they connect"} (<code>about:</code> in interface.yaml, or{" "}
          <code>application</code>/<code>vendor</code> in resources.yaml).
        </p>
      )}
    </div>
  );
}
