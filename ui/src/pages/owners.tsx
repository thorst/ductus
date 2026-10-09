import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { Download, Eye } from "lucide-react";
import { useState, type FormEvent } from "react";
import { previewReassign } from "@/api/actions";
import { ApiError, type Schemas } from "@/api/client";
import { owners } from "@/api/queries";
import { CopyButton } from "@/components/copy-button";
import { Loaded, PageHeader } from "@/components/page";
import { Button } from "@/components/ui/button";
import { Card, CardBody, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { count } from "@/lib/format";

/**
 * Who to call about what, and moving ownership when someone leaves or a team splits. The move is
 * previewed here and made in the project's repository (a patch or the CLI command), like any
 * other change to interface.yaml: the engine doesn't edit its own project.
 */
export function OwnersPage() {
  const query = useQuery(owners);
  return (
    <>
      <PageHeader title="Owners">
        Who owns each interface (<code>about.owner</code>), and destinations someone else owns.
      </PageHeader>
      <Loaded query={query}>
        {(groups) => (
          <div className="grid items-start gap-6 xl:grid-cols-[1fr_1fr]">
            <Groups groups={groups} />
            <Reassign groups={groups} />
          </div>
        )}
      </Loaded>
    </>
  );
}

function Groups({ groups }: { groups: Schemas["OwnerGroup"][] }) {
  return (
    <Card aria-label="Owners">
      <ul className="divide-y">
        {groups.map((g) => (
          <li key={g.owner ?? ""} className="px-4 py-3">
            <h2 className="text-sm font-semibold">
              {g.owner ?? <span className="text-warn">No owner</span>}{" "}
              <span className="font-normal text-muted">({count(g.owns.length)})</span>
            </h2>
            <ul className="mt-1 flex flex-wrap gap-x-4 gap-y-1 text-sm">
              {g.owns.map((o) => (
                <li key={`${o.interface}/${o.destination ?? ""}`}>
                  <Link to="/interfaces/$name" params={{ name: o.interface }}>
                    {o.interface}
                    {o.destination && ` → ${o.destination}`}
                  </Link>
                </li>
              ))}
            </ul>
            {!g.owner && (
              <p className="mt-2 text-xs text-muted">
                Name one with <code>about: {"{ owner: ... }"}</code> in interface.yaml.
              </p>
            )}
          </li>
        ))}
      </ul>
    </Card>
  );
}

function Reassign({ groups }: { groups: Schemas["OwnerGroup"][] }) {
  const named = groups.filter((g) => g.owner);
  const [from, setFrom] = useState(named[0]?.owner ?? "");
  const [to, setTo] = useState("");
  // Interfaces left out (all move by default): splitting one person's interfaces between two.
  const [leftOut, setLeftOut] = useState<Set<string>>(new Set());
  const [preview, setPreview] = useState<Schemas["ReassignPreview"] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const theirs = [...new Set(named.find((g) => g.owner === from)?.owns.map((o) => o.interface) ?? [])];

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const only = leftOut.size ? theirs.filter((i) => !leftOut.has(i)) : [];
      setPreview(await previewReassign(from, to.trim(), only));
    } catch (err) {
      setPreview(null);
      setError(err instanceof ApiError ? err.message : "The engine didn't answer.");
    } finally {
      setBusy(false);
    }
  };

  return (
    <Card aria-label="Move ownership">
      <CardHeader>
        <CardTitle>Move ownership</CardTitle>
      </CardHeader>
      <CardBody className="space-y-4">
        <p className="text-sm text-muted">
          Changes the <code>owner:</code> values in interface.yaml, keeping comments and layout. Preview it here, then
          apply the patch in the project's repository (or run the command there), review and deploy as usual.
        </p>
        <form onSubmit={submit} className="space-y-3">
          <div className="flex flex-wrap items-end gap-3">
            <label className="text-sm">
              <span className="block text-xs text-muted">From</span>
              <select
                className="mt-1 h-9 rounded-md border bg-panel px-2 text-sm"
                value={from}
                onChange={(e) => {
                  setFrom(e.target.value);
                  setLeftOut(new Set());
                  setPreview(null);
                }}
              >
                {named.map((g) => (
                  <option key={g.owner} value={g.owner ?? ""}>
                    {g.owner}
                  </option>
                ))}
              </select>
            </label>
            <label className="min-w-48 flex-1 text-sm">
              <span className="block text-xs text-muted">To</span>
              <Input className="mt-1" value={to} required onChange={(e) => setTo(e.target.value)} />
            </label>
            <Button type="submit" disabled={busy || !from || !to.trim()}>
              <Eye aria-hidden />
              {busy ? "Working…" : "Preview"}
            </Button>
          </div>
          {theirs.length > 1 && (
            <fieldset className="text-sm">
              <legend className="text-xs text-muted">Interfaces that move</legend>
              <div className="mt-1 flex flex-wrap gap-x-4 gap-y-1">
                {theirs.map((i) => (
                  <label key={i} className="flex items-center gap-1.5">
                    <input
                      type="checkbox"
                      checked={!leftOut.has(i)}
                      onChange={(e) => {
                        const next = new Set(leftOut);
                        if (e.target.checked) next.delete(i);
                        else next.add(i);
                        setLeftOut(next);
                        setPreview(null);
                      }}
                    />
                    {i}
                  </label>
                ))}
              </div>
            </fieldset>
          )}
        </form>
        {error && (
          <p role="alert" className="rounded-md border border-bad/40 bg-bad/10 p-3 text-sm text-bad">
            {error}
          </p>
        )}
        {preview && <Preview preview={preview} />}
      </CardBody>
    </Card>
  );
}

function Preview({ preview }: { preview: Schemas["ReassignPreview"] }) {
  const download = () => {
    const url = URL.createObjectURL(new Blob([preview.patch], { type: "text/x-diff" }));
    const a = document.createElement("a");
    a.href = url;
    a.download = "owners.patch";
    a.click();
    URL.revokeObjectURL(url);
  };
  return (
    <section aria-label="Preview" className="space-y-3">
      <p role="status" className="text-sm">
        {preview.changed > 0
          ? `${count(preview.changed)} owner ${preview.changed === 1 ? "value" : "values"} in ${count(preview.files.length)} ${preview.files.length === 1 ? "file" : "files"} would change. Nothing has been written.`
          : preview.refused.length === 0
            ? `No interface says that owner${preview.known.length ? ` (owners: ${preview.known.join(", ")})` : ""}.`
            : "Nothing can change until the files below are fixed."}
      </p>
      {preview.files.map((f) => (
        <div key={f.path} className="rounded-md border">
          <div className="border-b px-3 py-1.5 font-mono text-xs text-muted">{f.path}</div>
          <pre className="overflow-x-auto p-3 font-mono text-xs leading-relaxed">
            {f.lines.map((l) => (
              <span key={l.line} className="block">
                <span className="block text-bad">
                  {String(l.line).padStart(4)} - {l.before}
                </span>
                <span className="block text-ok">
                  {"".padStart(4)} + {l.after}
                </span>
              </span>
            ))}
          </pre>
        </div>
      ))}
      {preview.refused.map((r) => (
        <p key={r.path} role="alert" className="rounded-md border border-bad/40 bg-bad/10 p-3 text-sm">
          <span className="font-mono text-xs">{r.path}</span>: {r.error}
        </p>
      ))}
      {preview.patch && (
        <div className="space-y-3">
          <div className="flex flex-wrap gap-2">
            <Button variant="outline" onClick={download}>
              <Download aria-hidden />
              Download the patch
            </Button>
            <CopyButton text={preview.patch} what="the patch" />
          </div>
          <p className="text-xs text-muted">
            In the project's repository: <code>git apply owners.patch</code>, or run:
          </p>
          <div className="flex items-start gap-2">
            <code className="block min-w-0 flex-1 rounded-md border bg-bg p-2 font-mono text-xs break-all">
              {preview.command}
            </code>
            <CopyButton text={preview.command} what="the command" />
          </div>
        </div>
      )}
    </section>
  );
}
