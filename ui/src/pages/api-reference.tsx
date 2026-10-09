// The API reference, rendered from the engine's own OpenAPI document. Loaded only when opened
// (its own chunk: router.tsx), so it costs a first visit nothing.

import { useQuery } from "@tanstack/react-query";
import { Download } from "lucide-react";
import { Fragment, useState, type ReactNode } from "react";
import { CopyButton } from "@/components/copy-button";
import { Loaded, PageHeader } from "@/components/page";
import { Card, CardBody, CardHeader } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Table, Td } from "@/components/ui/table";
import {
  byTag,
  curl,
  flatten,
  resolve,
  typeTokens,
  type Document,
  type Endpoint,
  type Role,
  type Schema,
} from "@/lib/openapi";
import { cn } from "@/lib/utils";

const SPEC = "/api/v1/openapi.json";

const document = {
  queryKey: ["openapi"],
  queryFn: async () => {
    const r = await fetch(SPEC);
    if (!r.ok) throw new Error(`${r.status} ${r.statusText}`);
    return (await r.json()) as Document;
  },
  staleTime: Infinity,
};

const ROLE: Record<Role, string> = { none: "no token", viewer: "any token", operator: "operator", admin: "admin" };

export function ApiReferencePage() {
  const query = useQuery(document);
  const [filter, setFilter] = useState("");
  return (
    <>
      <PageHeader
        title="API reference"
        actions={
          <a href={SPEC} download="ductus-openapi.json" className="inline-flex items-center gap-1.5 text-sm">
            <Download aria-hidden className="size-4" />
            openapi.json
          </a>
        }
      >
        Everything the UI does goes through this API, so a script can do it too.
      </PageHeader>
      <Loaded query={query}>
        {(doc) => {
          const words = filter.toLowerCase().split(/\s+/).filter(Boolean);
          const groups = byTag(doc)
            .map((g) => ({
              ...g,
              endpoints: g.endpoints.filter((e) => {
                const text = `${e.method} ${e.path} ${e.op.summary ?? ""} ${g.tag}`.toLowerCase();
                return words.every((w) => text.includes(w));
              }),
            }))
            .filter((g) => g.endpoints.length > 0);
          return (
            <div className="space-y-6">
              <Intro doc={doc} />
              <label className="block max-w-md text-sm">
                <span className="text-xs text-muted">Find an endpoint</span>
                <Input
                  className="mt-1"
                  type="search"
                  value={filter}
                  placeholder="resend, drain, POST…"
                  onChange={(e) => setFilter(e.target.value)}
                />
              </label>
              <div className="grid items-start gap-6 xl:grid-cols-[14rem_1fr]">
                <nav aria-label="Endpoints" className="hidden text-sm xl:sticky xl:top-4 xl:block">
                  {groups.map((g) => (
                    <div key={g.tag} className="mb-3">
                      <div className="mb-1 text-xs font-medium tracking-wide text-muted uppercase">{g.tag}</div>
                      <ul className="space-y-0.5">
                        {g.endpoints.map((e) => (
                          <li key={e.id}>
                            <a href={`#${e.id}`} className="block truncate font-mono text-xs">
                              {e.method.toUpperCase()} {e.path}
                            </a>
                          </li>
                        ))}
                      </ul>
                    </div>
                  ))}
                </nav>
                <div className="min-w-0 space-y-8">
                  {groups.length === 0 && <p className="text-sm text-muted">No endpoint matches “{filter}”.</p>}
                  {groups.map((g) => (
                    <section key={g.tag} aria-label={g.tag} className="space-y-4">
                      <div>
                        <h2 className="text-lg font-semibold capitalize">{g.tag}</h2>
                        {g.description && <p className="text-sm text-muted">{g.description}</p>}
                      </div>
                      {g.endpoints.map((e) => (
                        <EndpointCard key={e.id} doc={doc} e={e} />
                      ))}
                    </section>
                  ))}
                  {!filter && <Schemas doc={doc} />}
                </div>
              </div>
            </div>
          );
        }}
      </Loaded>
    </>
  );
}

function Intro({ doc }: { doc: Document }) {
  const base = `${window.location.origin}/api/v1`;
  return (
    <Card>
      <CardBody className="space-y-2 text-sm">
        <p>
          <Prose text={doc.info.description ?? ""} />
        </p>
        <p>
          Base URL <code className="font-mono">{base}</code> · version {doc.info.version}: <code>/api/v1</code> stays
          compatible; a breaking change would come as <code>/api/v2</code>, with both served for a while.
        </p>
        <p>
          Roles: <strong>viewer</strong> reads, <strong>operator</strong> also acts on messages, destinations, drain and
          retention, <strong>admin</strong> also manages tokens. Examples read the token from <code>$DUCTUS_TOKEN</code>
          .
        </p>
      </CardBody>
    </Card>
  );
}

const METHOD_STYLE: Record<string, string> = {
  get: "border-ok/40 text-ok",
  post: "border-accent/50 text-accent",
  delete: "border-bad/40 text-bad",
};

function EndpointCard({ doc, e }: { doc: Document; e: Endpoint }) {
  const { op } = e;
  const params = op.parameters ?? [];
  const body = op.requestBody?.content["application/json"]?.schema;
  const base = `${window.location.origin}/api/v1`;
  const example = curl(doc, e, base);
  return (
    <Card id={e.id} aria-label={`${e.method.toUpperCase()} ${e.path}`} className="scroll-mt-4">
      <CardHeader className="flex-wrap justify-start">
        <span
          className={cn(
            "rounded border px-1.5 py-0.5 font-mono text-xs font-semibold uppercase",
            METHOD_STYLE[e.method] ?? "border-warn/40 text-warn",
          )}
        >
          {e.method}
        </span>
        <h3 className="font-mono text-sm font-semibold break-all">{e.path}</h3>
        <span className="ml-auto rounded-full border px-2 py-0.5 text-xs text-muted">{ROLE[e.role]}</span>
      </CardHeader>
      <CardBody className="space-y-4">
        {op.summary && (
          <p className="text-sm">
            <Prose text={op.summary} />
          </p>
        )}
        {params.length > 0 && (
          <Part title="Parameters">
            <Table>
              <tbody>
                {params.map((p) => (
                  <tr key={`${p.in}/${p.name}`}>
                    <Td className="w-0 font-mono text-xs whitespace-nowrap">
                      {p.name}
                      {p.required && <Required />}
                    </Td>
                    <Td className="w-0 text-xs whitespace-nowrap text-muted">
                      {p.in}, <Type schema={p.schema} />
                    </Td>
                    <Td className="text-xs">
                      <Prose text={p.description ?? ""} />
                    </Td>
                  </tr>
                ))}
              </tbody>
            </Table>
          </Part>
        )}
        {body && (
          <Part title="Request body">
            <Properties doc={doc} schema={body} />
          </Part>
        )}
        <Part title="Responses">
          <ul className="space-y-1 text-sm">
            {Object.entries(op.responses).map(([status, r]) => {
              const [type, content] = Object.entries(r.content ?? {})[0] ?? [];
              return (
                <li key={status} className="flex flex-wrap gap-x-2">
                  <span className={cn("font-mono font-semibold", status.startsWith("2") ? "text-ok" : "text-muted")}>
                    {status}
                  </span>
                  {content?.schema && <Type schema={content.schema} />}
                  {type && type !== "application/json" && <span className="text-muted">({type})</span>}
                  {r.description && <span className="text-muted">{r.description}</span>}
                </li>
              );
            })}
          </ul>
        </Part>
        <Part title="Example">
          <div className="flex items-start gap-2">
            <pre className="min-w-0 flex-1 overflow-x-auto rounded-md border bg-bg p-3 font-mono text-xs">
              {example}
            </pre>
            <CopyButton text={example} what={`the example for ${e.method.toUpperCase()} ${e.path}`} />
          </div>
        </Part>
      </CardBody>
    </Card>
  );
}

function Part({ title, children }: { title: string; children: ReactNode }) {
  return (
    <div>
      <h4 className="mb-1.5 text-xs font-medium tracking-wide text-muted uppercase">{title}</h4>
      {children}
    </div>
  );
}

const Required = () => (
  <span title="required" className="ml-0.5 text-bad">
    *<span className="sr-only"> (required)</span>
  </span>
);

/** A type, with named schemas linked to their description below. */
function Type({ schema }: { schema?: Schema }) {
  return (
    <span className="font-mono text-xs">
      {typeTokens(schema).map((t, n) =>
        typeof t === "string" ? (
          <Fragment key={n}>{t}</Fragment>
        ) : (
          <a key={n} href={`#schema-${t.ref}`}>
            {t.ref}
          </a>
        ),
      )}
    </span>
  );
}

/** An object's fields (one level: named types link to their own description). */
function Properties({ doc, schema }: { doc: Document; schema: Schema }) {
  const { name, schema: s } = resolve(doc, schema);
  const { properties, required } = flatten(doc, s);
  const fields = Object.entries(properties);
  if (fields.length === 0) return <Type schema={schema} />;
  return (
    <div>
      {name && (
        <div className="mb-1 text-xs">
          <Type schema={schema} />
        </div>
      )}
      <Table>
        <tbody>
          {fields.map(([field, f]) => (
            <tr key={field}>
              <Td className="w-0 font-mono text-xs whitespace-nowrap">
                {field}
                {required.includes(field) && <Required />}
              </Td>
              <Td className="w-0 text-xs whitespace-nowrap text-muted">
                <Type schema={f} />
              </Td>
              <Td className="text-xs">
                <Prose text={f.description ?? ""} />
              </Td>
            </tr>
          ))}
        </tbody>
      </Table>
    </div>
  );
}

function Schemas({ doc }: { doc: Document }) {
  const schemas = Object.entries(doc.components?.schemas ?? {}).sort(([a], [b]) => a.localeCompare(b));
  return (
    <section aria-label="Schemas" className="space-y-4">
      <h2 className="text-lg font-semibold">Schemas</h2>
      {schemas.map(([name, s]) => (
        <Card key={name} id={`schema-${name}`} aria-label={name} className="scroll-mt-4">
          <CardHeader>
            <h3 className="font-mono text-sm font-semibold">{name}</h3>
          </CardHeader>
          <CardBody className="space-y-2">
            {s.description && (
              <p className="text-sm">
                <Prose text={s.description} />
              </p>
            )}
            <Properties doc={doc} schema={s} />
          </CardBody>
        </Card>
      ))}
    </section>
  );
}

/** The descriptions' own markup: `code` in backticks, line breaks joined. */
function Prose({ text }: { text: string }) {
  return (
    <>
      {text
        .replace(/\s*\n\s*/g, " ")
        .split(/(`[^`]+`)/)
        .map((part, n) =>
          part.startsWith("`") && part.endsWith("`") && part.length > 1 ? (
            <code key={n} className="font-mono text-[0.92em]">
              {part.slice(1, -1)}
            </code>
          ) : (
            <Fragment key={n}>{part}</Fragment>
          ),
        )}
    </>
  );
}
