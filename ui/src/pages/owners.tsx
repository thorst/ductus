import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { CircleAlert, Download, Eye, UserCheck } from "lucide-react";
import { useState, type FormEvent } from "react";
import { previewReassign } from "@/api/actions";
import { ApiError, type Schemas } from "@/api/client";
import { ownerPeople, owners } from "@/api/queries";
import { CopyButton } from "@/components/copy-button";
import { OwnerReach, unreachable } from "@/components/owner";
import { Loaded, PageHeader } from "@/components/page";
import { Button } from "@/components/ui/button";
import { Card, CardBody, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { count } from "@/lib/format";

/**
 * Who to call about what (and whether each owner is a person with an account), and moving
 * ownership when someone leaves or a team splits. The move is previewed here and made in the
 * project's repository (a patch or the CLI command), like any other change to interface.yaml and
 * resources.yaml: the engine doesn't edit its own project.
 */
export function OwnersPage() {
  const query = useQuery(owners);
  return (
    <>
      <PageHeader title="Owners">
        Who owns each interface (<code>about.owner</code>), destinations someone else owns, and resources.yaml entries;
        and whether each owner is a person with an account.
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
  const lost = groups.filter(unreachable).length;
  return (
    <Card aria-label="Owners">
      {lost > 0 && (
        <p role="status" className="border-b px-4 py-3 text-sm text-warn">
          {count(lost)} {lost === 1 ? "owner reaches" : "owners reach"} nobody or the wrong person: make each an
          account's username or email, add it to <code>owners:</code> in ductus.yaml, or move what it owns (
          <code>ductus owners --check</code> says the same).
        </p>
      )}
      <ul className="divide-y">
        {groups.map((g) => (
          <li key={g.owner ?? ""} aria-label={g.owner ?? "No owner"} className="px-4 py-3">
            <h2 className="text-sm font-semibold">
              {g.owner ?? <span className="text-warn">No owner</span>}{" "}
              <span className="font-normal text-muted">({count(g.owns.length + g.resources.length)})</span>
            </h2>
            {g.owner && <OwnerReach group={g} className="mt-0.5 text-sm" />}
            <ul className="mt-1 flex flex-wrap gap-x-4 gap-y-1 text-sm">
              {g.owns.map((o) => (
                <li key={`${o.interface}/${o.destination ?? ""}`}>
                  <Link to="/interfaces/$name" params={{ name: o.interface }}>
                    {o.interface}
                    {o.destination && ` → ${o.destination}`}
                  </Link>
                </li>
              ))}
              {g.resources.map((r) => (
                <li key={`${r.kind}/${r.name}`}>
                  <span className="text-muted">{r.kind}</span> {r.name}
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
  const { data: people } = useQuery(ownerPeople);
  // Things left out (all move by default): splitting one person's work between two. Keys are
  // `i:<interface>` and `r:<resource>`.
  const [leftOut, setLeftOut] = useState<Set<string>>(new Set());
  const [preview, setPreview] = useState<Schemas["ReassignPreview"] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const group = named.find((g) => g.owner === from);
  const theirs = [
    ...[...new Set(group?.owns.map((o) => o.interface) ?? [])].map((i) => ({ key: `i:${i}`, name: i, label: i })),
    ...(group?.resources ?? []).map((r) => ({ key: `r:${r.name}`, name: r.name, label: `${r.kind} ${r.name}` })),
  ];
  const moving = theirs.filter((t) => !leftOut.has(t.key));
  // Offered as the new owner: a display name when it names one person only, else the email.
  const names = (people ?? []).map((p) => p.display_name ?? p.username);
  const targets = (people ?? []).map((p) => {
    const name = p.display_name ?? p.username;
    const value =
      names.filter((n) => n.toLowerCase() === name.toLowerCase()).length > 1 ? (p.email ?? p.username) : name;
    return { value, label: [p.username, p.email].filter(Boolean).join(" · ") };
  });

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const some = leftOut.size > 0;
      const only = (kind: string) => (some ? moving.filter((t) => t.key.startsWith(kind)).map((t) => t.name) : []);
      setPreview(await previewReassign(from, to.trim(), only("i:"), only("r:")));
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
          Changes the <code>owner:</code> values in interface.yaml and resources.yaml, keeping comments and layout.
          Preview it here, then apply the patch in the project's repository (or run the command there), review and
          deploy as usual.
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
              <Input
                className="mt-1"
                value={to}
                required
                list="owner-people"
                autoComplete="off"
                onChange={(e) => {
                  setTo(e.target.value);
                  setPreview(null);
                }}
              />
              <datalist id="owner-people">
                {targets.map((t) => (
                  <option key={t.value + t.label} value={t.value}>
                    {t.label}
                  </option>
                ))}
              </datalist>
            </label>
            <Button type="submit" disabled={busy || !from || !to.trim() || moving.length === 0}>
              <Eye aria-hidden />
              {busy ? "Working…" : "Preview"}
            </Button>
          </div>
          {theirs.length > 1 && (
            <fieldset className="text-sm">
              <legend className="text-xs text-muted">What moves</legend>
              <div className="mt-1 flex flex-wrap gap-x-4 gap-y-1">
                {theirs.map((i) => (
                  <label key={i.key} className="flex items-center gap-1.5">
                    <input
                      type="checkbox"
                      checked={!leftOut.has(i.key)}
                      onChange={(e) => {
                        const next = new Set(leftOut);
                        if (e.target.checked) next.delete(i.key);
                        else next.add(i.key);
                        setLeftOut(next);
                        setPreview(null);
                      }}
                    />
                    {i.label}
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
      {preview.warning ? (
        <p role="alert" className="flex items-start gap-2 rounded-md border border-warn/40 bg-warn/10 p-3 text-sm">
          <CircleAlert aria-hidden className="mt-0.5 size-4 shrink-0" />
          {preview.warning}
        </p>
      ) : preview.to_person ? (
        <p className="flex items-center gap-2 text-sm">
          <UserCheck aria-hidden className="size-4 shrink-0" />
          The new owner is {preview.to_person.display_name ?? preview.to_person.username}
          {preview.to_person.email ? ` (${preview.to_person.email})` : ""}, who has an account.
        </p>
      ) : null}
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
