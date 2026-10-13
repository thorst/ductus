import { useQuery } from "@tanstack/react-query";
import { getRouteApi, Link, useNavigate } from "@tanstack/react-router";
import { Check, Copy, Pencil, RotateCcw } from "lucide-react";
import { useMemo, useState } from "react";
import { editAndResend, resend, useCanOperate, useRefresh } from "@/api/actions";
import type { Schemas } from "@/api/client";
import { message } from "@/api/queries";
import { ActionDialog } from "@/components/action-dialog";
import { StatusBadge } from "@/components/health-badge";
import { Field, Loaded, PageHeader } from "@/components/page";
import { Button } from "@/components/ui/button";
import { Card, CardBody, CardHeader, CardTitle } from "@/components/ui/card";
import { Table, Td, Th } from "@/components/ui/table";
import { bytes, timestamp } from "@/lib/format";
import { fromEditable, isHl7, parseHl7, prettyJson, toEditable, type Hl7Field } from "@/lib/hl7";
import { cn } from "@/lib/utils";

const route = getRouteApi("/app/messages/$id");

export function MessagePage() {
  const { id } = route.useParams();
  const query = useQuery(message(id));
  const canOperate = useCanOperate();
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
            actions={canOperate && m.deliveries.length > 0 && m.payload != null && <EditButton m={m} />}
          >
            <Link to="/interfaces/$name" params={{ name: m.interface }} search={{ tab: "messages" }}>
              {m.interface}
            </Link>
            {" · "}received {timestamp(m.received_at)}
          </PageHeader>
          <div className="space-y-6">
            {m.copy_of != null && (
              <p role="note" className="rounded-md border border-accent/40 bg-accent-soft p-3 text-sm">
                An edited copy of{" "}
                <Link to="/messages/$id" params={{ id: m.copy_of }} className="font-mono">
                  message {m.copy_of}
                </Link>
                , queued by an operator. The original is kept as it was received.
              </p>
            )}
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
            <Deliveries m={m} canOperate={canOperate} />
            <History actions={m.actions} />
            {m.payload != null ? (
              <Payload payload={m.payload} />
            ) : (
              <Card aria-label="Payload">
                <CardHeader>
                  <CardTitle>Payload</CardTitle>
                </CardHeader>
                <CardBody>
                  <p className="text-sm text-muted">
                    You don&rsquo;t have permission to read payloads (they hold patient data). Ask an admin for the{" "}
                    <strong className="text-text">payloads</strong> permission.
                  </p>
                </CardBody>
              </Card>
            )}
          </div>
        </>
      )}
    </Loaded>
  );
}

function Deliveries({ m, canOperate }: { m: Schemas["MessageDetail"]; canOperate: boolean }) {
  const [open, setOpen] = useState<string | null>(null);
  const [resending, setResending] = useState<Schemas["DeliveryInfo"] | null>(null);
  const refresh = useRefresh();
  if (m.deliveries.length === 0) return null;
  return (
    <Card aria-label="Destinations">
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
            {canOperate && <Th>Action</Th>}
          </tr>
        </thead>
        <tbody>
          {m.deliveries.map((d) => (
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
                    {m.payload_hidden ? "hidden" : "not saved"}
                  </span>
                )}
              </Td>
              {canOperate && (
                <Td>
                  {/* Pending: being sent or retried already. */}
                  {d.status !== "pending" && (
                    <Button size="sm" variant="outline" onClick={() => setResending(d)}>
                      <RotateCcw aria-hidden />
                      {d.status === "quarantined" ? "Resend" : "Replay"}
                      <span className="sr-only"> to {d.destination}</span>
                    </Button>
                  )}
                </Td>
              )}
            </tr>
          ))}
        </tbody>
      </Table>
      {resending && (
        <ActionDialog
          open
          onOpenChange={(o) => !o && setResending(null)}
          title={`${resending.status === "quarantined" ? "Resend" : "Replay"} message ${m.id} to ${resending.destination}`}
          description={
            resending.status === "quarantined" ? (
              <>
                It goes back in {resending.destination}&rsquo;s queue in its place: sent next for its key, through
                today&rsquo;s code.
              </>
            ) : (
              <>
                {resending.destination} already has it ({resending.status}). It&rsquo;s sent again as it is: the same
                message and control id, so a receiver that drops repeats may ignore it. Newer messages for the same key
                sent since arrive before it.
              </>
            )
          }
          confirm={resending.status === "quarantined" ? "Resend" : "Replay"}
          onConfirm={async (why) => {
            await resend(m.id, [resending.destination], why);
            await refresh();
          }}
        />
      )}
    </Card>
  );
}

/** Correct a message: a copy with the edits is queued; the original stays as received. */
function EditButton({ m }: { m: Schemas["MessageDetail"] }) {
  const [open, setOpen] = useState(false);
  const payload = m.payload ?? "";
  const hl7 = isHl7(payload);
  const [text, setText] = useState(() => toEditable(payload));
  const quarantined = m.deliveries.filter((d) => d.status === "quarantined").map((d) => d.destination);
  const [chosen, setChosen] = useState<string[]>(quarantined);
  const refresh = useRefresh();
  const navigate = useNavigate();
  return (
    <>
      <Button
        variant="outline"
        onClick={() => {
          setText(toEditable(payload));
          setChosen(quarantined);
          setOpen(true);
        }}
      >
        <Pencil aria-hidden />
        Edit and resend
      </Button>
      <ActionDialog
        open={open}
        onOpenChange={setOpen}
        wide
        title={`Edit and resend message ${m.id}`}
        description="A corrected copy is queued as a new message, at the back of its lane. The original stays as it was received; its quarantined deliveries to the destinations you pick are marked replaced."
        confirm="Queue the copy"
        onConfirm={async (why) => {
          const copy = await editAndResend(m.id, fromEditable(text, hl7), chosen, why);
          await refresh();
          await navigate({ to: "/messages/$id", params: { id: copy.id } });
        }}
      >
        <label className="block text-sm">
          <span className="text-muted">{hl7 ? "Message (one segment per line)" : "Message"}</span>
          <textarea
            value={text}
            onChange={(e) => setText(e.target.value)}
            spellCheck={false}
            rows={Math.min(20, Math.max(6, text.split("\n").length + 1))}
            className="mt-1 w-full rounded-md border bg-bg p-3 font-mono text-xs focus-visible:border-accent"
          />
        </label>
        <fieldset className="text-sm">
          <legend className="text-muted">Send the copy to</legend>
          <div className="mt-1 flex flex-wrap gap-4">
            {m.deliveries.map((d) => (
              <label key={d.destination} className="flex items-center gap-2">
                <input
                  type="checkbox"
                  // Pending: the original may be being sent right now; the engine refuses it.
                  disabled={d.status === "pending"}
                  checked={chosen.includes(d.destination)}
                  onChange={(e) =>
                    setChosen(e.target.checked ? [...chosen, d.destination] : chosen.filter((c) => c !== d.destination))
                  }
                />
                {d.destination} <span className="text-muted">({d.status})</span>
              </label>
            ))}
          </div>
        </fieldset>
      </ActionDialog>
    </>
  );
}

const BEFORE: Record<string, string> = { quarantined: "Resent", sent: "Replayed", filtered: "Replayed" };

/** What people did to this message, oldest first. */
function History({ actions }: { actions: Schemas["ActionInfo"][] }) {
  if (actions.length === 0) return null;
  return (
    <Card aria-label="History">
      <CardHeader>
        <CardTitle>History</CardTitle>
      </CardHeader>
      <Table>
        <thead>
          <tr>
            <Th>When</Th>
            <Th>What</Th>
            <Th>Destination</Th>
            <Th>Before</Th>
            <Th>By</Th>
            <Th>Reason</Th>
          </tr>
        </thead>
        <tbody>
          {actions.map((a, n) => (
            <tr key={n}>
              <Td className="tabular-nums">{timestamp(a.at)}</Td>
              <Td>
                {a.action === "edit" ? (
                  <>
                    Edited: copy{" "}
                    <Link to="/messages/$id" params={{ id: a.copy! }} className="font-mono">
                      {a.copy}
                    </Link>
                  </>
                ) : (
                  (BEFORE[a.previous_status ?? ""] ?? "Resent")
                )}
              </Td>
              <Td>{a.destination}</Td>
              <Td>
                {a.previous_status ? (
                  <span className="flex flex-wrap items-center gap-2">
                    <StatusBadge status={a.previous_status} />
                    <span className="text-xs text-muted">
                      {a.previous_attempts} {a.previous_attempts === 1 ? "attempt" : "attempts"}
                    </span>
                  </span>
                ) : (
                  <span className="text-muted">none</span>
                )}
                {a.previous_error && <div className="mt-1 text-xs break-words text-muted">{a.previous_error}</div>}
              </Td>
              <Td>{a.by}</Td>
              <Td className="text-xs">{a.reason ?? ""}</Td>
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
