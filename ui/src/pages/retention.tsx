import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { TriangleAlert } from "lucide-react";
import { useState } from "react";
import { confirmRetention, useCanOperate, useRefresh } from "@/api/actions";
import type { Schemas } from "@/api/client";
import { retention } from "@/api/queries";
import { ActionDialog } from "@/components/action-dialog";
import { Loaded, PageHeader } from "@/components/page";
import { Button } from "@/components/ui/button";
import { Card, CardBody, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Table, Td, Th } from "@/components/ui/table";
import { ago, bytes, count, timestamp } from "@/lib/format";

/** "7 days" as the engine's windows are written in interface.yaml: 1w, 3w, 7w. */
const weeks = (days: number) => (days % 7 === 0 ? `${days / 7}w (${days} days)` : `${days} days`);

/**
 * How long each interface keeps its messages, changes on their way, a shorter window waiting
 * for someone to confirm it (it deletes messages), and what retention had to keep anyway.
 */
export function RetentionPage() {
  const query = useQuery(retention);
  return (
    <>
      <PageHeader title="Retention">
        Each interface keeps its messages for 1, 3 or 7 weeks (<code>retention:</code> in interface.yaml). A shorter
        window deletes messages, so it waits until someone confirms it.
      </PageHeader>
      <Loaded query={query}>
        {(r) => (
          <div className="space-y-6">
            <Interfaces interfaces={r.interfaces} />
            <HeldArea held={r.held} lastDrop={r.last_drop_at} />
            {r.kept.length > 0 && <Kept kept={r.kept} />}
          </div>
        )}
      </Loaded>
    </>
  );
}

function Interfaces({ interfaces }: { interfaces: Schemas["InterfaceRetention"][] }) {
  const canOperate = useCanOperate();
  const [confirming, setConfirming] = useState<Schemas["InterfaceRetention"] | null>(null);
  const waiting = interfaces.filter((i) => i.needs_confirmation);
  return (
    <>
      {waiting.length > 0 && (
        <p role="status" className="flex gap-2 rounded-md border border-warn/40 bg-warn/10 p-3 text-sm">
          <TriangleAlert aria-hidden className="mt-0.5 size-4 shrink-0 text-warn" />
          {waiting.length === 1
            ? `${waiting[0]?.name} has a shorter retention waiting for a confirmation.`
            : `${waiting.length} interfaces have a shorter retention waiting for a confirmation.`}
        </p>
      )}
      <Card>
        <Table aria-label="Retention by interface">
          <thead>
            <tr>
              <Th>Interface</Th>
              <Th>Keeps</Th>
              <Th>Configured</Th>
              <Th>Change</Th>
              {canOperate && <Th className="w-0" />}
            </tr>
          </thead>
          <tbody>
            {interfaces.map((i) => (
              <tr key={i.name}>
                <Td>
                  <Link to="/interfaces/$name" params={{ name: i.name }} className="font-medium">
                    {i.name}
                  </Link>
                </Td>
                <Td className="tabular-nums">{weeks(i.days)}</Td>
                <Td className="tabular-nums">{weeks(i.configured)}</Td>
                <Td className="text-sm">
                  <ChangeText i={i} />
                </Td>
                {canOperate && (
                  <Td>
                    {i.needs_confirmation && (
                      <Button size="sm" onClick={() => setConfirming(i)}>
                        Confirm<span className="sr-only"> {i.name}'s shorter retention</span>
                      </Button>
                    )}
                  </Td>
                )}
              </tr>
            ))}
          </tbody>
        </Table>
      </Card>
      {confirming && <ConfirmShorter key={confirming.name} i={confirming} onClose={() => setConfirming(null)} />}
    </>
  );
}

function ChangeText({ i }: { i: Schemas["InterfaceRetention"] }) {
  if (i.change)
    return (
      <>
        Changing from {i.change.from} to {i.change.to} days, started {ago(i.change.started_at)}
        {i.change.rows_left != null && <> · about {count(i.change.rows_left)} messages left to move</>}
        {i.waiting != null && !i.needs_confirmation && <> · then {i.waiting} days</>}
      </>
    );
  if (i.needs_confirmation && i.waiting != null)
    return (
      <span className="font-medium text-warn">
        Shorter ({i.waiting} days) waits for a confirmation
        {i.would_delete != null && <>: deletes {count(i.would_delete)} messages now</>}
      </span>
    );
  if (i.waiting != null) return <>{i.waiting} days next</>;
  return <span className="text-muted">–</span>;
}

/** Deleting messages can't be undone: the interface's name is typed, not just clicked. */
function ConfirmShorter({ i, onClose }: { i: Schemas["InterfaceRetention"]; onClose: () => void }) {
  const [typed, setTyped] = useState("");
  const refresh = useRefresh();
  const deletes = i.would_delete != null ? `${count(i.would_delete)} messages` : "its messages";
  return (
    <ActionDialog
      open
      onOpenChange={(open) => !open && onClose()}
      reason={false}
      title={`Shorten ${i.name}'s retention to ${i.waiting} days`}
      description={
        <>
          This deletes {deletes} older than {i.waiting} days, with their deliveries, and can't be undone. Messages still
          waiting are kept until delivered; quarantined ones are never deleted by it.
        </>
      }
      confirm="Delete and shorten"
      disabled={typed !== i.name}
      onConfirm={async () => {
        await confirmRetention(i.name);
        await refresh();
      }}
    >
      <label className="block text-sm">
        <span className="text-muted">
          Type <strong className="text-text">{i.name}</strong> to confirm
        </span>
        <Input className="mt-1" value={typed} onChange={(e) => setTyped(e.target.value)} autoComplete="off" />
      </label>
    </ActionDialog>
  );
}

function HeldArea({ held, lastDrop }: { held: Schemas["Held"]; lastDrop?: string | null }) {
  return (
    <Card aria-label="Held area">
      <CardHeader>
        <CardTitle>Held area</CardTitle>
        <span className="text-xs text-muted">
          as of the last hourly drop, {lastDrop ? timestamp(lastDrop) : "none yet"}
        </span>
      </CardHeader>
      <CardBody className="space-y-3 text-sm">
        <p className="text-muted">
          Messages past their retention that someone still needs: waiting for a destination, or quarantined and kept
          until resolved. A month goes once nothing in it is unfinished.
        </p>
        {held.messages === 0 ? (
          <p>Nothing held.</p>
        ) : (
          <>
            <p>
              {count(held.messages)} messages ({bytes(held.bytes)}): {count(held.pending)} waiting,{" "}
              {held.quarantined > 0 ? (
                <Link to="/quarantine" search={{}}>
                  {count(held.quarantined)} quarantined
                </Link>
              ) : (
                "none quarantined"
              )}
              .
            </p>
            {held.interfaces.length > 0 && (
              <ul className="list-inside list-disc">
                {held.interfaces.map((h) => (
                  <li key={h.name}>
                    <Link to="/interfaces/$name" params={{ name: h.name }} className="underline">
                      {h.name}
                    </Link>
                    : {count(h.pending)} waiting, {count(h.quarantined)} quarantined
                  </li>
                ))}
              </ul>
            )}
            <Table aria-label="Held months">
              <thead>
                <tr>
                  <Th>Month</Th>
                  <Th className="text-right">Messages</Th>
                  <Th className="text-right">Waiting</Th>
                  <Th className="text-right">Quarantined</Th>
                  <Th className="text-right">Disk</Th>
                </tr>
              </thead>
              <tbody>
                {held.months.map((m) => (
                  <tr key={m.month}>
                    <Td>{m.month}</Td>
                    <Td className="text-right tabular-nums">{count(m.messages)}</Td>
                    <Td className="text-right tabular-nums">{count(m.pending)}</Td>
                    <Td className="text-right tabular-nums">{count(m.quarantined)}</Td>
                    <Td className="text-right tabular-nums">{bytes(m.bytes)}</Td>
                  </tr>
                ))}
              </tbody>
            </Table>
          </>
        )}
      </CardBody>
    </Card>
  );
}

function Kept({ kept }: { kept: Schemas["KeptPartition"][] }) {
  return (
    <Card aria-label="Kept partitions">
      <CardHeader>
        <CardTitle>Expired, but kept</CardTitle>
        <span className="text-xs text-muted">waiting for a longer retention's change to move their messages</span>
      </CardHeader>
      <Table>
        <thead>
          <tr>
            <Th>Partition</Th>
            <Th className="text-right">Tier</Th>
            <Th className="text-right">Waiting</Th>
            <Th className="text-right">Quarantined</Th>
            <Th>Kept for</Th>
          </tr>
        </thead>
        <tbody>
          {kept.map((k) => (
            <tr key={k.partition}>
              <Td className="font-mono text-xs">{k.partition}</Td>
              <Td className="text-right tabular-nums">{k.tier} days</Td>
              <Td className="text-right tabular-nums">{count(k.pending)}</Td>
              <Td className="text-right tabular-nums">{count(k.quarantined)}</Td>
              <Td>{k.kept_for.join(", ")}</Td>
            </tr>
          ))}
        </tbody>
      </Table>
    </Card>
  );
}
