import { ExternalLink, TriangleAlert } from "lucide-react";
import { Field } from "@/components/page";
import {
  dateText,
  isWebUrl,
  NO_VALUE,
  texts,
  valuesFor,
  type DocsEntry,
  type DocsField,
  type DocsReport,
} from "@/lib/docs";
import { cn } from "@/lib/utils";

const chip = "inline-flex items-center rounded-full border px-2 py-0.5 text-xs font-medium";

/** A required value that isn't there: a word and an icon, so color never says it alone. */
export function Missing() {
  return (
    <span className={cn(chip, "gap-1 border-warn/40 bg-warn/10 text-warn")}>
      <TriangleAlert aria-hidden className="size-3.5" />
      Missing
    </span>
  );
}

/** One field's value the way its type reads best; a required one with no value is "Missing", else a dash. */
export function DocValue({ field, entry }: { field: DocsField; entry: DocsEntry }) {
  const values = texts(entry.custom[field.key]);
  if (values.length === 0) return field.required ? <Missing /> : <span className="text-muted">–</span>;
  switch (field.type) {
    case "enum":
      return <span className={chip}>{values[0]}</span>;
    case "list":
      return (
        <span className="flex flex-wrap gap-1">
          {values.map((v) => (
            <span key={v} className={chip}>
              {v}
            </span>
          ))}
        </span>
      );
    case "url":
      if (!isWebUrl(values[0] ?? "")) return <>{values[0]}</>;
      return (
        <a
          href={values[0]}
          target="_blank"
          rel="noopener noreferrer"
          className="inline-flex items-center gap-1 break-all"
        >
          {values[0]}
          <ExternalLink aria-hidden className="size-3 shrink-0" />
          <span className="sr-only"> (opens in a new tab)</span>
        </a>
      );
    case "date":
      return <time dateTime={values[0]}>{dateText(values[0] ?? "")}</time>;
    default:
      return <>{values[0]}</>;
  }
}

/** A definition list of an entry's fields (the ones that apply at its level). */
export function DocFields({ fields, entry, className }: { fields: DocsField[]; entry: DocsEntry; className?: string }) {
  return (
    <dl className={cn("grid grid-cols-2 gap-x-6 gap-y-3 md:grid-cols-4", className)}>
      {fields.map((f) => (
        <Field key={f.key} label={f.title}>
          <DocValue field={f} entry={entry} />
        </Field>
      ))}
    </dl>
  );
}

const select = "h-9 rounded-md border bg-panel px-2 text-sm focus-visible:border-accent";

/** "Show those where <field> is <value>": built from the company's fields. */
export function DocsFilter({
  report,
  field,
  value,
  onChange,
}: {
  report: DocsReport;
  field?: string;
  value?: string;
  onChange: (field?: string, value?: string) => void;
}) {
  const chosen = report.fields.find((f) => f.key === field);
  return (
    <div className="flex flex-wrap items-center gap-2 text-sm">
      <label className="flex items-center gap-2">
        Documentation field
        <select
          aria-label="Documentation field"
          className={select}
          value={chosen?.key ?? ""}
          onChange={(e) => onChange(e.target.value || undefined, undefined)}
        >
          <option value="">Any</option>
          {report.fields.map((f) => (
            <option key={f.key} value={f.key}>
              {f.title}
            </option>
          ))}
        </select>
      </label>
      {chosen && (
        <label className="flex items-center gap-2">
          is
          <select
            aria-label="Documentation value"
            className={select}
            value={value ?? ""}
            onChange={(e) => onChange(chosen.key, e.target.value || undefined)}
          >
            <option value="">Anything</option>
            <option value={NO_VALUE}>No value</option>
            {valuesFor(chosen, report.entries).map((v) => (
              <option key={v} value={v}>
                {v}
              </option>
            ))}
          </select>
        </label>
      )}
    </div>
  );
}
