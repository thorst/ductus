import { useQuery } from "@tanstack/react-query";
import { getRouteApi, Link } from "@tanstack/react-router";
import { Check, Copy } from "lucide-react";
import { useMemo, useState } from "react";
import type { Schemas } from "@/api/client";
import { message } from "@/api/queries";
import { StatusBadge } from "@/components/health-badge";
import { Field, Loaded, PageHeader } from "@/components/page";
import { Button } from "@/components/ui/button";
import { Card, CardBody, CardHeader, CardTitle } from "@/components/ui/card";
import { Table, Td, Th } from "@/components/ui/table";
import { bytes, timestamp } from "@/lib/format";
import { isHl7, parseHl7, prettyJson, type Hl7Field } from "@/lib/hl7";
import { cn } from "@/lib/utils";

const route = getRouteApi("/app/messages/$id");

export function MessagePage() {
  const { id } = route.useParams();
  const query = useQuery(message(id));
  return (
    <Loaded query={query}>
      {(m) => (
        <>
          <PageHeader
            title={
              <span className="flex flex-wrap items-center gap-3">
                Message <span className="font-mono">{m.id}</span>
                <StatusBadge status={m.status} />
              </span>
            }
          >
            <Link to="/interfaces/$name" params={{ name: m.interface }} search={{ tab: "messages" }}>
              {m.interface}
            </Link>
            {" · "}received {timestamp(m.received_at)}
          </PageHeader>
          <div className="space-y-6">
            <Card>
              <CardBody>
                <dl className="grid grid-cols-2 gap-x-6 gap-y-4 md:grid-cols-4">
                  <Field label="Control id">{m.control_id ?? "–"}</Field>
                  <Field label="Ordering key">{m.ordering_key ?? "–"}</Field>
                  <Field label="Size">{bytes(m.size)}</Field>
                  <Field label="Last change">{timestamp(m.updated_at)}</Field>
                  {m.origin != null && (
                    <Field label="Routed from">
                      <Link to="/messages/$id" params={{ id: m.origin }} className="font-mono">
                        message {m.origin}
                      </Link>
                    </Field>
                  )}
                  {/* With destinations, their table has each one's error. */}
                  {m.last_error && m.deliveries.length === 0 && (
                    <div className="col-span-2 md:col-span-4">
                      <Field label="Error">{m.last_error}</Field>
                    </div>
                  )}
                </dl>
              </CardBody>
            </Card>
            <Deliveries deliveries={m.deliveries} />
            <Payload payload={m.payload} />
          </div>
        </>
      )}
    </Loaded>
  );
}

function Deliveries({ deliveries }: { deliveries: Schemas["DeliveryInfo"][] }) {
  const [open, setOpen] = useState<string | null>(null);
  if (deliveries.length === 0) return null;
  return (
    <Card>
      <CardHeader>
        <CardTitle>Destinations</CardTitle>
      </CardHeader>
      <Table>
        <thead>
          <tr>
            <Th>Destination</Th>
            <Th>Status</Th>
            <Th className="text-right">Attempts</Th>
            <Th>Error</Th>
            <Th>Sent</Th>
          </tr>
        </thead>
        <tbody>
          {deliveries.map((d) => (
            <tr key={d.destination}>
              <Td className="font-medium">{d.destination}</Td>
              <Td>
                <StatusBadge status={d.status} />
              </Td>
              <Td className="text-right tabular-nums">{d.attempts}</Td>
              <Td className="max-w-md text-xs break-words">{d.last_error ?? ""}</Td>
              <Td>
                {d.output != null ? (
                  <>
                    <Button
                      size="sm"
                      variant="outline"
                      onClick={() => setOpen(open === d.destination ? null : d.destination)}
                    >
                      {open === d.destination ? "Hide output" : "Show output"}
                    </Button>
                    {open === d.destination && <PayloadText text={d.output} className="mt-2" />}
                  </>
                ) : (
                  <span className="text-xs text-muted" title="Saved only for destinations with save_output: true">
                    not saved
                  </span>
                )}
              </Td>
            </tr>
          ))}
        </tbody>
      </Table>
    </Card>
  );
}

function Payload({ payload }: { payload: string }) {
  const hl7 = isHl7(payload);
  const json = useMemo(() => prettyJson(payload), [payload]);
  const [raw, setRaw] = useState(false);
  return (
    <Card>
      <CardHeader>
        <CardTitle>Payload</CardTitle>
        <div className="flex gap-2">
          {(hl7 || json) && (
            <Button size="sm" variant="outline" aria-pressed={raw} onClick={() => setRaw(!raw)}>
              {raw ? "Formatted" : "Raw"}
            </Button>
          )}
          <CopyButton text={payload} />
        </div>
      </CardHeader>
      <CardBody>
        {raw || (!hl7 && !json) ? (
          <PayloadText text={payload} />
        ) : hl7 ? (
          <Hl7View payload={payload} />
        ) : (
          <PayloadText text={json!} />
        )}
      </CardBody>
    </Card>
  );
}

function PayloadText({ text, className }: { text: string; className?: string }) {
  return (
    <pre
      className={cn("max-h-[60vh] overflow-auto rounded-md bg-bg p-3 font-mono text-xs whitespace-pre-wrap", className)}
    >
      {text.replace(/\r\n?/g, "\n")}
    </pre>
  );
}

/**
 * One segment per line. Pointing at a value shows its path (PID-3.1) in the line above: one
 * listener for the whole message, however many fields it has.
 */
function Hl7View({ payload }: { payload: string }) {
  const segments = useMemo(() => parseHl7(payload), [payload]);
  const [pointed, setPointed] = useState<{ path: string; value: string } | null>(null);
  const point = (target: EventTarget) => {
    const part = target instanceof HTMLElement ? target.closest<HTMLElement>("[data-path]") : null;
    if (part) setPointed({ path: part.dataset.path!, value: part.textContent ?? "" });
  };
  return (
    <div>
      <p aria-live="polite" className="mb-2 h-5 truncate text-xs text-muted">
        {pointed ? (
          <>
            <span className="font-mono font-semibold text-text">{pointed.path}</span> = {pointed.value}
          </>
        ) : (
          "Point at a value to see its field."
        )}
      </p>
      <div
        className="max-h-[60vh] overflow-auto rounded-md bg-bg p-3 font-mono text-xs leading-6"
        onMouseOver={(e) => point(e.target)}
        onClick={(e) => point(e.target)}
      >
        {segments.map((segment, n) => (
          <div key={n} className="break-all">
            <span className="font-semibold text-accent">{segment.name}</span>
            {segment.fields.map((field, f) =>
              segment.name === "MSH" && f === 0 ? null : (
                <span key={f}>
                  <span className="text-muted">|</span>
                  <FieldView field={field} />
                </span>
              ),
            )}
          </div>
        ))}
      </div>
    </div>
  );
}

function FieldView({ field }: { field: Hl7Field }) {
  if (field.value === "") return null;
  if (field.components.length === 0) return <Part path={field.path} value={field.value} />;
  return (
    <>
      {field.components.map((c, n) => (
        <span key={c.path}>
          {n > 0 && <span className="text-muted">^</span>}
          {c.value && <Part path={c.path} value={c.value} />}
        </span>
      ))}
    </>
  );
}

function Part({ path, value }: { path: string; value: string }) {
  return (
    <span data-path={path} className="rounded-sm hover:bg-accent-soft">
      {value}
    </span>
  );
}

function CopyButton({ text }: { text: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <Button
      size="sm"
      variant="outline"
      onClick={async () => {
        await navigator.clipboard.writeText(text);
        setCopied(true);
        setTimeout(() => setCopied(false), 1500);
      }}
    >
      {copied ? <Check aria-hidden /> : <Copy aria-hidden />}
      {copied ? "Copied" : "Copy"}
    </Button>
  );
}
