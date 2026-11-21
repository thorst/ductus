// Non-HL7 formats: CSV, fixed-length and XML, plus decoding/encoding by an interface's format.

import { type Message, parse as parseHl7 } from "./hl7.ts";

// ---------------------------------------------------------------------------
// Format specs (from interface.yaml, passed in by the engine)

export interface FixedField {
  name: string;
  /** 0-based start column. */
  start: number;
  width: number;
  align?: "left" | "right";
  pad?: string;
}

export interface FixedLayout {
  fields: FixedField[];
  /** Trim padding when reading (default true). */
  trim?: boolean;
}

/** `record`: each message is one record (a file split with `split: records`), so `transform`
 *  receives that record rather than a list of one. */
export type FormatSpec =
  | { kind: "hl7" | "json" | "text" }
  | { kind: "xml"; names?: XmlNames }
  | { kind: "csv"; delimiter: string; header: boolean; record?: boolean }
  | ({ kind: "fixed"; record?: boolean } & FixedLayout);

// ---------------------------------------------------------------------------
// CSV (RFC 4180: quoted fields may contain delimiters, quotes ("") and newlines)

export type CsvRecord = Record<string, string>;

function parseCsvRows(text: string, delimiter: string): string[][] {
  const rows: string[][] = [];
  let row: string[] = [];
  let field = "";
  let quoted = false;
  let i = 0;
  const endField = () => {
    row.push(field);
    field = "";
  };
  const endRow = () => {
    endField();
    rows.push(row);
    row = [];
  };
  while (i < text.length) {
    const c = text[i];
    if (quoted) {
      if (c === '"') {
        if (text[i + 1] === '"') {
          field += '"';
          i += 2;
          continue;
        }
        quoted = false;
      } else {
        field += c;
      }
      i++;
      continue;
    }
    if (c === '"' && field === "") quoted = true;
    else if (c === delimiter) endField();
    else if (c === "\n") endRow();
    else if (c === "\r") {
      endRow();
      if (text[i + 1] === "\n") i++;
    } else field += c;
    i++;
  }
  if (field !== "" || row.length > 0) endRow();
  return rows;
}

/** Parse CSV. With a header (default), records are objects keyed by column name. */
export function parseCsv(text: string, options?: { delimiter?: string; header?: true }): CsvRecord[];
export function parseCsv(text: string, options: { delimiter?: string; header: false }): string[][];
export function parseCsv(text: string, options: { delimiter?: string; header?: boolean } = {}): CsvRecord[] | string[][] {
  const rows = parseCsvRows(text, options.delimiter ?? ",");
  if (options.header === false) return rows;
  const [header = [], ...data] = rows;
  return data.map((r) => Object.fromEntries(header.map((h, i) => [h, r[i] ?? ""])));
}

function csvField(value: unknown, delimiter: string): string {
  const s = value === null || value === undefined ? "" : String(value);
  return s.includes(delimiter) || s.includes('"') || s.includes("\n") || s.includes("\r")
    ? `"${s.replaceAll('"', '""')}"`
    : s;
}

/** Write CSV. Objects get a header row (columns in first-seen order); arrays are written as-is. */
export function toCsv(records: readonly (CsvRecord | readonly unknown[])[], options: { delimiter?: string; header?: boolean } = {}): string {
  const d = options.delimiter ?? ",";
  if (records.length === 0) return "";
  if (Array.isArray(records[0])) {
    return records.map((r) => (r as unknown[]).map((v) => csvField(v, d)).join(d)).join("\n") + "\n";
  }
  const columns: string[] = [];
  for (const r of records as CsvRecord[]) for (const k of Object.keys(r)) if (!columns.includes(k)) columns.push(k);
  const lines = (records as CsvRecord[]).map((r) => columns.map((c) => csvField(r[c], d)).join(d));
  if (options.header !== false) lines.unshift(columns.map((c) => csvField(c, d)).join(d));
  return lines.join("\n") + "\n";
}

// ---------------------------------------------------------------------------
// Fixed-length

/** Parse fixed-length lines into records keyed by field name. */
export function parseFixed(text: string, layout: FixedLayout): CsvRecord[] {
  const trim = layout.trim !== false;
  return text
    .split(/\r?\n/)
    .filter((line) => line.length > 0)
    .map((line) =>
      Object.fromEntries(
        layout.fields.map((f) => {
          const raw = line.slice(f.start, f.start + f.width);
          return [f.name, trim ? raw.trim() : raw];
        }),
      )
    );
}

/** Write records as fixed-length lines. Values longer than their field are cut off. */
export function toFixed(records: readonly Record<string, unknown>[], layout: FixedLayout): string {
  const length = Math.max(...layout.fields.map((f) => f.start + f.width));
  return records
    .map((r) => {
      const line = new Array<string>(length).fill(" ");
      for (const f of layout.fields) {
        const pad = f.pad ?? " ";
        const value = String(r[f.name] ?? "").slice(0, f.width);
        const cell = f.align === "right" ? value.padStart(f.width, pad) : value.padEnd(f.width, pad);
        for (let i = 0; i < f.width; i++) line[f.start + i] = cell[i];
      }
      return line.join("");
    })
    .join("\n") + "\n";
}

// ---------------------------------------------------------------------------
// XML

const ENTITIES: Record<string, string> = { lt: "<", gt: ">", amp: "&", quot: '"', apos: "'" };

function decodeEntities(s: string): string {
  if (!s.includes("&")) return s;
  return s.replace(/&(#x[0-9a-fA-F]+|#[0-9]+|[a-zA-Z]+);/g, (m, e: string) => {
    if (e[0] === "#") return String.fromCodePoint(e[1] === "x" ? parseInt(e.slice(2), 16) : parseInt(e.slice(1), 10));
    return ENTITIES[e] ?? m;
  });
}

const escapeText = (s: string) => s.replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;");
const escapeAttr = (s: string) => escapeText(s).replaceAll('"', "&quot;");

/**
 * How names in paths match an XML document's (the format's `names`): `local` by the name without
 * its namespace prefix, on both sides (`Patient` matches `<hl7:Patient>`); `qualified` exactly as
 * written.
 */
export type XmlNames = "local" | "qualified";

const localName = (name: string) => name.slice(name.lastIndexOf(":") + 1);

function sameName(names: XmlNames, path: string, document: string): boolean {
  return names === "qualified" ? path === document : localName(path) === localName(document);
}

const isDeclaration = (attribute: string) => attribute === "xmlns" || attribute.startsWith("xmlns:");

/** `[@root=1.2.3]` (an attribute) or `[Code=GLU]` (a child element's text). */
interface XmlFilter {
  attribute: boolean;
  name: string;
  value: string;
}

/** One step of a path: an element (the n-th from 1, or the first a filter matches) or `@attribute`. */
type XmlStep = { name: string; index: number; filters?: undefined } | { name: string; index?: undefined; filters: XmlFilter[] };

/** `text` split at `sep` where it's outside brackets and quotes. */
export function splitOutside(text: string, sep: string): string[] {
  const parts: string[] = [];
  let start = 0;
  let depth = 0;
  let quote: string | undefined;
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (quote !== undefined) {
      if (c === quote) quote = undefined;
    } else if (c === "'" || c === '"') quote = c;
    else if (c === "[") depth++;
    else if (c === "]") depth--;
    else if (c === sep && depth === 0) {
      parts.push(text.slice(start, i));
      start = i + 1;
    }
  }
  parts.push(text.slice(start));
  return parts;
}

/** Where the `]` closing a bracket is (`text` starts inside it), past quotes and inner brackets; -1 if none. */
export function closingBracket(text: string): number {
  let quote: string | undefined;
  let depth = 0;
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (quote !== undefined) {
      if (c === quote) quote = undefined;
    } else if (c === "'" || c === '"') quote = c;
    else if (c === "[") depth++;
    else if (c === "]") {
      if (depth === 0) return i;
      depth--;
    }
  }
  return -1;
}

/** A filter's value: quoted (`'A|B'`) or as written. */
export function filterValue(text: string, fail: (why: string) => never): string {
  const q = text[0];
  if (q === "'" || q === '"') {
    const inner = text.slice(1, -1);
    if (text.length < 2 || !text.endsWith(q) || inner.includes(q)) fail(`a quoted value runs from one quote to the next: \`${text}\``);
    return inner;
  }
  if (/['"[\]]/.test(text)) fail(`quote a value with brackets or quotes in it: \`${text}\``);
  return text;
}

/**
 * An XML path, the syntax keys take (`crates/ductus-core/src/keys.rs`, checked against it by
 * `sdk/tests/key-paths.json`): element names between `/`, `[n]` from 1, `[@attr=value]` or
 * `[Child=value]` filters with `|` between alternatives, and `@attr` as the last step.
 */
function xmlPath(path: string): { steps: XmlStep[]; attribute?: string } {
  const fail = (why: string): never => {
    throw new Error(
      `invalid XML path "${path}": ${why} (e.g. "Patient/MRN", "Item[2]/@code", "id[@root=2.16.840.1]/@extension")`,
    );
  };
  const name = (text: string) => {
    if (text === "" || /[\s[\]=|'"@/]/.test(text)) fail(`\`${text}\` isn't an element or attribute name`);
    return text;
  };
  const parts = splitOutside(path, "/").filter((p) => p.length > 0);
  const steps: XmlStep[] = [];
  let attribute: string | undefined;
  parts.forEach((part, i) => {
    if (part.startsWith("@")) {
      if (i !== parts.length - 1) fail(`\`${part}\` must be the last step`);
      attribute = name(part.slice(1));
      return;
    }
    const open = part.indexOf("[");
    if (open < 0) {
      steps.push({ name: name(part), index: 1 });
      return;
    }
    const inside = part.slice(open + 1);
    const end = closingBracket(inside);
    if (end < 0) fail("a `[` without its `]`");
    if (end + 1 !== inside.length) fail(`\`${inside.slice(end + 1)}\` after \`]\``);
    const bracket = inside.slice(0, end);
    if (/^[0-9]*$/.test(bracket)) {
      const n = Number(bracket);
      if (bracket === "" || n < 1) fail("XML positions count from 1");
      steps.push({ name: name(part.slice(0, open)), index: n });
      return;
    }
    const filters = splitOutside(bracket, "|").map((alt): XmlFilter => {
      const eq = alt.indexOf("=");
      if (eq < 0) fail(`a filter is \`@attr=value\`: \`${alt}\``);
      const on = alt.slice(0, eq);
      const attr = on.startsWith("@");
      return { attribute: attr, name: name(attr ? on.slice(1) : on), value: filterValue(alt.slice(eq + 1), fail) };
    });
    steps.push({ name: name(part.slice(0, open)), filters });
  });
  if (steps.length === 0 && attribute === undefined) fail("no steps");
  return { steps, attribute };
}

/**
 * An XML element. Paths are relative to it: `doc.get("Patient/MRN")`, `"Order[2]/Id"`, `"@id"`,
 * `"id[@root=2.16.840.1]/@extension"`. Names match by their local name unless the document was
 * parsed with `names: "qualified"` (the interface's `format: { xml: { names: qualified } }`).
 */
export class XmlElement {
  attributes: Record<string, string>;
  children: (XmlElement | string)[];
  /** The `<?xml ...?>` declaration, kept on the document root. */
  declaration?: string;
  /** How names in paths match (from `parseXml`; elements `add` and `set` create keep their parent's). */
  names: XmlNames = "local";

  constructor(public name: string, attributes: Record<string, string> = {}, children: (XmlElement | string)[] = []) {
    this.attributes = attributes;
    this.children = children;
  }

  /** Text directly inside this element. Setting it replaces the element's content. */
  get text(): string {
    return this.children.filter((c): c is string => typeof c === "string").join("");
  }

  set text(value: string) {
    this.children = [value];
  }

  /** Child elements, optionally only those with a name. */
  elements(name?: string): XmlElement[] {
    return this.children.filter((c): c is XmlElement =>
      c instanceof XmlElement && (name === undefined || sameName(this.names, name, c.name))
    );
  }

  /** First child element with this name. */
  element(name: string): XmlElement | undefined {
    return this.elements(name)[0];
  }

  /** An attribute's name as the element has it (matched as names are); namespace declarations never. */
  private attributeName(name: string): string | undefined {
    return Object.keys(this.attributes).find((k) => !isDeclaration(k) && sameName(this.names, name, k));
  }

  private matches(filter: XmlFilter): boolean {
    let value: string;
    if (filter.attribute) {
      const at = this.attributeName(filter.name);
      value = at === undefined ? "" : this.attributes[at];
    } else value = this.element(filter.name)?.text ?? "";
    return value === filter.value;
  }

  private walk(steps: XmlStep[]): XmlElement | undefined {
    let current: XmlElement | undefined = this;
    for (const step of steps) {
      const candidates: XmlElement[] = current.elements(step.name);
      if (step.filters === undefined) current = candidates[step.index - 1];
      else {
        current = undefined;
        for (const f of step.filters) {
          current = candidates.find((e) => e.matches(f));
          if (current) break;
        }
      }
      if (!current) return undefined;
    }
    return current;
  }

  /** The element at a path (no `@attribute` step), or undefined. */
  find(path: string): XmlElement | undefined {
    const { steps, attribute } = xmlPath(path);
    if (attribute !== undefined) throw new Error(`find("${path}"): find gives elements; get("${path}") reads an attribute`);
    return this.walk(steps);
  }

  /** Text at a path, or an attribute with a final `@name` step. Missing reads as "". */
  get(path: string): string {
    const { steps, attribute } = xmlPath(path);
    const owner = this.walk(steps);
    if (!owner) return "";
    if (attribute === undefined) return owner.text;
    const at = owner.attributeName(attribute);
    return at === undefined ? "" : owner.attributes[at];
  }

  /** Set text at a path (or an attribute with `@name`), creating missing elements. */
  set(path: string, value: string): void {
    const { steps, attribute } = xmlPath(path);
    let current: XmlElement = this;
    for (const step of steps) {
      if (step.filters !== undefined) {
        throw new Error(`set("${path}"): set takes names and positions; find the element first: find("…[…]")?.set(…)`);
      }
      const matches = current.elements(step.name);
      while (matches.length < step.index) matches.push(current.add(step.name));
      current = matches[step.index - 1];
    }
    if (attribute !== undefined) current.attributes[current.attributeName(attribute) ?? attribute] = value;
    else current.text = value;
  }

  /** Append a child element. */
  add(name: string, text?: string, attributes: Record<string, string> = {}): XmlElement {
    const child = new XmlElement(name, attributes, text === undefined ? [] : [text]);
    child.names = this.names;
    this.children.push(child);
    return child;
  }

  /** Remove child elements with this name. */
  remove(name: string): void {
    this.children = this.children.filter((c) => !(c instanceof XmlElement && sameName(this.names, name, c.name)));
  }

  toString(): string {
    const attrs = Object.entries(this.attributes).map(([k, v]) => ` ${k}="${escapeAttr(v)}"`).join("");
    const inner = this.children.map((c) => (typeof c === "string" ? escapeText(c) : c.toString())).join("");
    const body = inner.length === 0 ? `<${this.name}${attrs}/>` : `<${this.name}${attrs}>${inner}</${this.name}>`;
    return this.declaration ? `${this.declaration}\n${body}` : body;
  }
}

/**
 * Parse an XML document into its root element. Comments and processing instructions are dropped.
 * `names`: how paths match its names (default `local`, the prefix ignored).
 */
export function parseXml(text: string, options: { names?: XmlNames } = {}): XmlElement {
  const names = options.names ?? "local";
  let i = 0;
  const fail = (what: string): never => {
    throw new Error(`invalid XML at position ${i}: ${what}`);
  };
  const skipSpace = () => {
    while (i < text.length && /\s/.test(text[i])) i++;
  };
  const readName = () => {
    const m = /^[^\s/>=]+/.exec(text.slice(i, i + 256));
    if (!m) fail("expected a name");
    i += m![0].length;
    return m![0];
  };

  let declaration: string | undefined;
  // Prolog: BOM, <?xml?>, comments, doctype.
  if (text.charCodeAt(0) === 0xfeff) i = 1;
  for (;;) {
    skipSpace();
    if (text.startsWith("<?", i)) {
      const end = text.indexOf("?>", i);
      if (end < 0) fail("unclosed <?");
      if (text.startsWith("<?xml", i)) declaration = text.slice(i, end + 2);
      i = end + 2;
    } else if (text.startsWith("<!--", i)) {
      const end = text.indexOf("-->", i);
      if (end < 0) fail("unclosed comment");
      i = end + 3;
    } else if (text.startsWith("<!DOCTYPE", i)) {
      const end = text.indexOf(">", i);
      if (end < 0) fail("unclosed DOCTYPE");
      i = end + 1;
    } else break;
  }

  const parseElement = (): XmlElement => {
    if (text[i] !== "<") fail("expected <");
    i++;
    const el = new XmlElement(readName());
    el.names = names;
    for (;;) {
      skipSpace();
      if (text.startsWith("/>", i)) {
        i += 2;
        return el;
      }
      if (text[i] === ">") {
        i++;
        break;
      }
      const attr = readName();
      skipSpace();
      if (text[i] !== "=") fail(`expected = after attribute ${attr}`);
      i++;
      skipSpace();
      const quote = text[i];
      if (quote !== '"' && quote !== "'") fail("expected a quoted attribute value");
      const end = text.indexOf(quote, i + 1);
      if (end < 0) fail("unclosed attribute value");
      el.attributes[attr] = decodeEntities(text.slice(i + 1, end));
      i = end + 1;
    }
    // Content
    for (;;) {
      if (i >= text.length) fail(`unclosed <${el.name}>`);
      if (text.startsWith("</", i)) {
        i += 2;
        const name = readName();
        if (name !== el.name) fail(`</${name}> closes <${el.name}>`);
        skipSpace();
        if (text[i] !== ">") fail("expected >");
        i++;
        return el;
      }
      if (text.startsWith("<![CDATA[", i)) {
        const end = text.indexOf("]]>", i);
        if (end < 0) fail("unclosed CDATA");
        el.children.push(text.slice(i + 9, end));
        i = end + 3;
      } else if (text.startsWith("<!--", i)) {
        const end = text.indexOf("-->", i);
        if (end < 0) fail("unclosed comment");
        i = end + 3;
      } else if (text.startsWith("<?", i)) {
        const end = text.indexOf("?>", i);
        if (end < 0) fail("unclosed <?");
        i = end + 2;
      } else if (text[i] === "<") {
        el.children.push(parseElement());
      } else {
        const end = text.indexOf("<", i);
        const raw = text.slice(i, end < 0 ? text.length : end);
        // Whitespace between elements is formatting, not content.
        if (raw.trim().length > 0 || el.children.length === 0 && end >= 0 && text.startsWith(`</${el.name}`, end)) {
          el.children.push(decodeEntities(raw));
        }
        i = end < 0 ? text.length : end;
      }
    }
  };

  const root = parseElement();
  root.declaration = declaration;
  return root;
}

// ---------------------------------------------------------------------------
// Decoding/encoding by format

/** Parse a raw payload as the given format (what `transform` receives). */
export function decode(raw: string, spec: FormatSpec): unknown {
  switch (spec.kind) {
    case "hl7":
      return parseHl7(raw);
    case "json":
      return JSON.parse(raw);
    case "xml":
      return parseXml(raw, { names: spec.names });
    case "csv": {
      const records = parseCsv(raw, { delimiter: spec.delimiter, header: spec.header as true });
      return spec.record ? records[0] : records;
    }
    case "fixed": {
      const records = parseFixed(raw, spec);
      return spec.record ? records[0] : records;
    }
    case "text":
      return raw;
  }
}

/** A single object is one record (e.g. a transform handling one record of a split file). */
function asRecords(result: unknown, what: string): unknown[] {
  if (Array.isArray(result)) return result;
  if (typeof result === "object" && result !== null) return [result];
  throw new TypeError(`${what} output needs a record or an array of records`);
}

/** Serialize a transform's result for the given output format. */
export function encode(result: unknown, spec: FormatSpec, isMessage: (v: unknown) => v is Message): string {
  if (typeof result === "string") return result;
  if (isMessage(result) || result instanceof XmlElement) return result.toString();
  switch (spec.kind) {
    case "json":
      return JSON.stringify(result);
    case "csv":
      return toCsv(asRecords(result, "CSV") as CsvRecord[], { delimiter: spec.delimiter, header: spec.header });
    case "fixed":
      return toFixed(asRecords(result, "fixed-length") as Record<string, unknown>[], spec);
    case "text":
      return String(result);
    case "hl7":
      throw new TypeError("HL7 output needs a Message (or a string); set `output: json` to send objects as JSON");
    case "xml":
      throw new TypeError("XML output needs an XmlElement (or a string)");
  }
}
