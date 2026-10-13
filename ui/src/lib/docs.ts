// The company-defined documentation fields (docs-schema.yaml): reading values the way the
// screens show and filter them. The engine has already said what's missing and what's wrong.

import type { Schemas } from "@/api/client";

export type DocsReport = Schemas["DocsReport"];
export type DocsField = Schemas["DocsField"];
export type DocsEntry = Schemas["DocsEntry"];

/** The filter value for "this field has no value". */
export const NO_VALUE = "__no_value__";

/** What a value says, as a list of texts: a text is one, a list is its items, nothing is none. */
export function texts(value: unknown): string[] {
  if (typeof value === "string") return value.trim() ? [value.trim()] : [];
  if (Array.isArray(value)) return value.flatMap(texts);
  return [];
}

export const hasValue = (entry: DocsEntry, key: string) => texts(entry.custom[key]).length > 0;

/** The fields that apply at a level (an entry's `level`), in the order the file lists them. */
export const fieldsFor = (report: DocsReport, level: string) =>
  report.fields.filter((f) => f.applies_to.includes(level));

/** Does the entry match "this field has this value" (or has no value)? Other levels never do. */
export function matches(entry: DocsEntry, field: DocsField, value: string): boolean {
  if (!field.applies_to.includes(entry.level)) return false;
  if (value === NO_VALUE) return !hasValue(entry, field.key);
  return texts(entry.custom[field.key]).includes(value);
}

/** What to offer when filtering on a field: the declared values of an enum, else what's in use. */
export function valuesFor(field: DocsField, entries: DocsEntry[]): string[] {
  if (field.values.length > 0) return field.values;
  const used = new Set(
    entries.filter((e) => field.applies_to.includes(e.level)).flatMap((e) => texts(e.custom[field.key])),
  );
  return [...used].sort((a, b) => a.localeCompare(b));
}

/** Per required field: how many of the entries it applies to lack it. */
export function missingCounts(report: DocsReport) {
  return report.fields
    .filter((f) => f.required)
    .map((field) => {
      const applicable = report.entries.filter((e) => field.applies_to.includes(e.level));
      return { field, missing: applicable.filter((e) => e.missing.includes(field.key)).length, of: applicable.length };
    });
}

/** The entry for an interface, a destination (`interface.destination`) or a resource name. */
export const entryNamed = (report: DocsReport, level: string, name: string) =>
  report.entries.find((e) => e.level === level && e.name === name);

/** A date as the person who wrote it reads it; anything else as written. */
export function dateText(value: string): string {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(value);
  if (!m) return value;
  const date = new Date(Date.UTC(Number(m[1]), Number(m[2]) - 1, Number(m[3])));
  return date.toLocaleDateString("en-US", { year: "numeric", month: "short", day: "numeric", timeZone: "UTC" });
}

/** Only http(s) addresses become links: the engine runs with values `ductus validate` would reject. */
export const isWebUrl = (text: string) => /^https?:\/\/[^\s/]/i.test(text);
