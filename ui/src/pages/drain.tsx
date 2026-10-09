import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { CircleCheck, CirclePause, CirclePlay, LoaderCircle, Power } from "lucide-react";
import { useState } from "react";
import { endDrain, startDrain, useCanOperate, useRefresh } from "@/api/actions";
import type { Schemas } from "@/api/client";
import { drain } from "@/api/queries";
import { ActionDialog } from "@/components/action-dialog";
import { Loaded, PageHeader } from "@/components/page";
import { Button } from "@/components/ui/button";
import { Card, CardBody } from "@/components/ui/card";
import { ago, count } from "@/lib/format";

/**
 * Draining before an upgrade or a server move: the engine stops taking messages (senders keep
 * theirs) and delivers what it has stored, then, if asked, exits.
 */
export function DrainPage() {
  const query = useQuery(drain);
  // An engine asked to stop once drained goes away: say that instead of "didn't answer" (a failed
  // refetch keeps the last answer).
  const stopped = query.error && query.data?.stop_when_drained;
  return (
    <>
      <PageHeader title="Drain">
        Before an upgrade or a server move: stop taking messages, deliver everything stored, then stop. Senders keep
        what they couldn't send and send it to the engine that comes back.
      </PageHeader>
      {stopped ? (
        <p role="status" className="flex gap-2 rounded-md border border-ok/40 bg-ok/10 p-4 text-sm">
          <Power aria-hidden className="mt-0.5 size-4 shrink-0 text-ok" />
          The engine drained and stopped, as asked. Start it again (or the new version) to take messages.
        </p>
      ) : (
        <Loaded query={query}>{(d) => <DrainState d={d} />}</Loaded>
      )}
    </>
  );
}

function DrainState({ d }: { d: Schemas["DrainStatus"] }) {
  const canOperate = useCanOperate();
  const refresh = useRefresh();
  const [dialog, setDialog] = useState<"start" | "stop" | "resume" | null>(null);
  const [stop, setStop] = useState(false);
  const close = (open: boolean) => !open && setDialog(null);
  return (
    <div className="space-y-6">
      <Card aria-label="Drain state">
        <CardBody className="space-y-4">
          <p role="status" className="flex items-start gap-2 text-sm">
            <StateIcon state={d.state} />
            <span>
              <Headline d={d} />
              {d.stop_when_drained && d.state !== "accepting" && <strong> The engine stops once drained.</strong>}
            </span>
          </p>
          {d.waiting_for.length > 0 && (
            <div className="text-sm">
              <p>
                <span className="text-muted">Still delivering for: </span>
                {d.waiting_for.map((name, n) => (
                  <span key={name}>
                    {n > 0 && ", "}
                    <Link to="/interfaces/$name" params={{ name }}>
                      {name}
                    </Link>
                  </span>
                ))}
              </p>
              <p className="mt-1 text-xs text-muted">
                A destination that's down or a stopped interface keeps a drain from finishing: see{" "}
                <Link to="/hotspots" search={{}}>
                  Hotspots
                </Link>
                .
              </p>
            </div>
          )}
          {d.paused.length > 0 && (
            <p className="text-sm">
              <span className="text-muted">Paused (they keep their messages and don't hold the drain up): </span>
              {d.paused.join(", ")}
            </p>
          )}
          {canOperate && (
            <div className="flex flex-wrap gap-2 border-t pt-4">
              {d.state === "accepting" ? (
                <Button onClick={() => setDialog("start")}>
                  <CirclePause aria-hidden />
                  Start draining
                </Button>
              ) : (
                <>
                  <Button variant="outline" onClick={() => setDialog("resume")}>
                    <CirclePlay aria-hidden />
                    Take messages again
                  </Button>
                  {!d.stop_when_drained && (
                    <Button variant="outline" onClick={() => setDialog("stop")}>
                      <Power aria-hidden />
                      {d.state === "drained" ? "Stop the engine" : "Stop when drained"}
                    </Button>
                  )}
                </>
              )}
            </div>
          )}
        </CardBody>
      </Card>
      <ActionDialog
        open={dialog === "start"}
        onOpenChange={close}
        reason={false}
        title="Start draining"
        description="MLLP listeners close (senders queue on their side), queued HTTP answers 503 with Retry-After, file pickup and schedules pause. Everything stored is still delivered. A restart takes messages again: draining isn't remembered."
        confirm={stop ? "Drain, then stop" : "Drain"}
        onConfirm={async () => {
          await startDrain(stop);
          await refresh();
        }}
      >
        <label className="flex items-start gap-2 text-sm">
          <input type="checkbox" className="mt-1" checked={stop} onChange={(e) => setStop(e.target.checked)} />
          <span>
            Stop the engine when drained
            <span className="block text-xs text-muted">
              For an upgrade or a server move: the engine exits by itself.
            </span>
          </span>
        </label>
      </ActionDialog>
      <ActionDialog
        open={dialog === "stop"}
        onOpenChange={close}
        reason={false}
        title={d.state === "drained" ? "Stop the engine" : "Stop when drained"}
        description={
          d.state === "drained"
            ? "Everything is delivered: the engine exits now. It takes messages again when it's started."
            : `The engine exits once the ${count(d.pending)} deliveries still to go are done.`
        }
        confirm={d.state === "drained" ? "Stop it" : "Stop when drained"}
        onConfirm={async () => {
          await startDrain(true);
          await refresh();
        }}
      />
      <ActionDialog
        open={dialog === "resume"}
        onOpenChange={close}
        reason={false}
        title="Take messages again"
        description="Listeners open again, file pickup and schedules carry on."
        confirm="Take messages"
        onConfirm={async () => {
          await endDrain();
          await refresh();
        }}
      />
    </div>
  );
}

function StateIcon({ state }: { state: string }) {
  if (state === "drained") return <CircleCheck aria-hidden className="mt-0.5 size-5 shrink-0 text-ok" />;
  if (state === "draining") return <LoaderCircle aria-hidden className="mt-0.5 size-5 shrink-0 text-warn" />;
  return <CirclePlay aria-hidden className="mt-0.5 size-5 shrink-0 text-ok" />;
}

function Headline({ d }: { d: Schemas["DrainStatus"] }) {
  if (d.state === "drained")
    return (
      <>
        <strong>Drained</strong> {ago(d.since)}: no new messages are taken, and everything stored has been delivered.
      </>
    );
  if (d.state === "draining")
    return (
      <>
        <strong>Draining</strong> (started {ago(d.since)}): no new messages are taken; {count(d.pending)}{" "}
        {d.pending === 1 ? "delivery" : "deliveries"} still to go.
      </>
    );
  return (
    <>
      <strong>Taking messages.</strong> {count(d.pending)} {d.pending === 1 ? "delivery" : "deliveries"} waiting.
    </>
  );
}
