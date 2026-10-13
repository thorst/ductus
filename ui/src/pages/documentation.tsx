import { useQuery } from "@tanstack/react-query";
import { getRouteApi, Link, useNavigate } from "@tanstack/react-router";
import { docs } from "@/api/queries";
import { DocsFilter, DocValue } from "@/components/docs-values";
import { Loaded, PageHeader } from "@/components/page";
import { Table, Td, Th } from "@/components/ui/table";
import { fieldsFor, matches, missingCounts, NO_VALUE, type DocsEntry, type DocsReport } from "@/lib/docs";

const route = getRouteApi("/app/documentation");

const LEVELS = [
  { level: "interface", title: "Interfaces" },
  { level: "destination", title: "Destinations" },
  { level: "resource", title: "Resources" },
] as const;

/** Every interface, destination and resource with the company's own documentation fields. */
export function DocumentationPage() {
  const { field, value } = route.useSearch();
  const navigate = useNavigate({ from: "/documentation" });
  const query = useQuery(docs);
  return (
    <>
      <PageHeader title="Documentation">
        The fields your company defined in <code className="font-mono text-xs">docs-schema.yaml</code>, and what each
        interface, destination and resource says. Change them in the project's files: the repository is the source of
        truth.
      </PageHeader>
      <Loaded query={query}>
        {(report) =>
          report.fields.length === 0 ? (
            <p className="text-sm">
              No fields are defined yet. Declare them in <code className="font-mono text-xs">docs-schema.yaml</code>{" "}
              (next to <code className="font-mono text-xs">ductus.yaml</code>) and give interfaces a{" "}
              <code className="font-mono text-xs">custom:</code> block.{" "}
              <code className="font-mono text-xs">ductus docs</code> shows the same in a terminal.
            </p>
          ) : (
            <div className="space-y-6">
              <Gaps report={report} onPick={(key) => void navigate({ search: { field: key, value: NO_VALUE } })} />
              <DocsFilter
                report={report}
                field={field}
                value={value}
                onChange={(f, v) => void navigate({ search: { field: f, value: v } })}
              />
              {LEVELS.map(({ level, title }) => (
                <LevelTable key={level} report={report} level={level} title={title} field={field} value={value} />
              ))}
            </div>
          )
        }
      </Loaded>
    </>
  );
}

/** Per required field, how many lack it; a click filters to them. */
function Gaps({ report, onPick }: { report: DocsReport; onPick: (key: string) => void }) {
  const counts = missingCounts(report);
  if (counts.length === 0) return null;
  return (
    <ul aria-label="Required fields" className="flex flex-wrap gap-3">
      {counts.map(({ field, missing, of }) => (
        <li key={field.key} className="rounded-lg border bg-panel px-4 py-3 text-sm">
          <div className="text-xs text-muted">{field.title} (required)</div>
          {missing === 0 ? (
            <div>All {of} have it</div>
          ) : (
            <button type="button" className="font-semibold text-warn underline" onClick={() => onPick(field.key)}>
              {missing} of {of} missing
            </button>
          )}
        </li>
      ))}
    </ul>
  );
}

function LevelTable({
  report,
  level,
  title,
  field,
  value,
}: {
  report: DocsReport;
  level: string;
  title: string;
  field?: string;
  value?: string;
}) {
  const fields = fieldsFor(report, level);
  const chosen = report.fields.find((f) => f.key === field);
  const rows = report.entries
    .filter((e) => e.level === level)
    .filter((e) => !chosen || !value || matches(e, chosen, value));
  if (fields.length === 0) return null;
  return (
    <section aria-label={title}>
      <h2 className="mb-2 text-base font-semibold">
        {title} <span className="text-sm font-normal text-muted">{rows.length}</span>
      </h2>
      {rows.length === 0 ? (
        <p className="text-sm text-muted">None match.</p>
      ) : (
        <div className="rounded-lg border bg-panel">
          <Table>
            <thead>
              <tr>
                <Th>Name</Th>
                {fields.map((f) => (
                  <Th key={f.key}>{f.title}</Th>
                ))}
              </tr>
            </thead>
            <tbody>
              {rows.map((e) => (
                <tr key={e.name}>
                  <Td className="font-medium">
                    <EntryName entry={e} />
                  </Td>
                  {fields.map((f) => (
                    <Td key={f.key}>
                      <DocValue field={f} entry={e} />
                    </Td>
                  ))}
                </tr>
              ))}
            </tbody>
          </Table>
        </div>
      )}
    </section>
  );
}

function EntryName({ entry }: { entry: DocsEntry }) {
  if (entry.level === "interface") {
    return (
      <Link to="/interfaces/$name" params={{ name: entry.name }}>
        {entry.name}
      </Link>
    );
  }
  return <>{entry.name}</>;
}
