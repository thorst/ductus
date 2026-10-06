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

export type FormatSpec =
  | { kind: "hl7" | "json" | "xml" | "text" }
  | { kind: "csv"; delimiter: string; header: boolean }
  | ({ kind: "fixed" } & FixedLayout);

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

/** One step of a path: `Name`, `Name[2]` (1-based), or `@attribute` (last step only). */
function splitPath(path: string): { name: string; index: number }[] {
  return path.split("/").filter((p) => p.length > 0).map((p) => {
    const m = /^([^[\]]+)(?:\[(\d+)\])?$/.exec(p);
    if (!m) throw new Error(`invalid XML path step "${p}"`);
    return { name: m[1], index: m[2] === undefined ? 1 : Number(m[2]) };
  });
}

/** An XML element. Paths are relative to the element: `doc.get("Patient/MRN")`, `"Order[2]/Id"`, `"@id"`. */
export class XmlElement {
  attributes: Record<string, string>;
  children: (XmlElement | string)[];
  /** The `<?xml ...?>` declaration, kept on the document root. */
  declaration?: string;

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
    return this.children.filter((c): c is XmlElement => c instanceof XmlElement && (name === undefined || c.name === name));
  }

  /** First child element with this name. */
  element(name: string): XmlElement | undefined {
    return this.elements(name)[0];
  }

  /** The element at a path (no `@attribute` step), or undefined. */
  find(path: string): XmlElement | undefined {
    let current: XmlElement | undefined = this;
    for (const step of splitPath(path)) {
      current = current?.elements(step.name)[step.index - 1];
      if (!current) return undefined;
    }
    return current;
  }

  /** Text at a path, or an attribute with a final `@name` step. Missing reads as "". */
  get(path: string): string {
    const steps = path.split("/");
    const last = steps[steps.length - 1];
    if (last.startsWith("@")) {
      const owner = steps.length > 1 ? this.find(steps.slice(0, -1).join("/")) : this;
      return owner?.attributes[last.slice(1)] ?? "";
    }
    return this.find(path)?.text ?? "";
  }

  /** Set text at a path (or an attribute with `@name`), creating missing elements. */
  set(path: string, value: string): void {
    const steps = path.split("/").filter((s) => s.length > 0);
    const last = steps[steps.length - 1];
    const elementSteps = last.startsWith("@") ? steps.slice(0, -1) : steps;
    let current: XmlElement = this;
    for (const step of splitPath(elementSteps.join("/"))) {
      const matches = current.elements(step.name);
      while (matches.length < step.index) {
        const created = new XmlElement(step.name);
        current.children.push(created);
        matches.push(created);
      }
      current = matches[step.index - 1];
    }
    if (last.startsWith("@")) current.attributes[last.slice(1)] = value;
    else current.text = value;
  }

  /** Append a child element. */
  add(name: string, text?: string, attributes: Record<string, string> = {}): XmlElement {
    const child = new XmlElement(name, attributes, text === undefined ? [] : [text]);
    this.children.push(child);
    return child;
  }

  /** Remove child elements with this name. */
  remove(name: string): void {
    this.children = this.children.filter((c) => !(c instanceof XmlElement && c.name === name));
  }

  toString(): string {
    const attrs = Object.entries(this.attributes).map(([k, v]) => ` ${k}="${escapeAttr(v)}"`).join("");
    const inner = this.children.map((c) => (typeof c === "string" ? escapeText(c) : c.toString())).join("");
    const body = inner.length === 0 ? `<${this.name}${attrs}/>` : `<${this.name}${attrs}>${inner}</${this.name}>`;
    return this.declaration ? `${this.declaration}\n${body}` : body;
  }
}

/** Parse an XML document into its root element. Comments and processing instructions are dropped. */
export function parseXml(text: string): XmlElement {
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
      return parseXml(raw);
    case "csv":
      return parseCsv(raw, { delimiter: spec.delimiter, header: spec.header as true });
    case "fixed":
      return parseFixed(raw, spec);
    case "text":
      return raw;
  }
}

/** Serialize a transform's result for the given output format. */
export function encode(result: unknown, spec: FormatSpec, isMessage: (v: unknown) => v is Message): string {
  if (typeof result === "string") return result;
  if (isMessage(result) || result instanceof XmlElement) return result.toString();
  switch (spec.kind) {
    case "json":
      return JSON.stringify(result);
    case "csv":
      if (!Array.isArray(result)) throw new TypeError("CSV output needs an array of records");
      return toCsv(result, { delimiter: spec.delimiter, header: spec.header });
    case "fixed":
      if (!Array.isArray(result)) throw new TypeError("fixed-length output needs an array of records");
      return toFixed(result, spec);
    case "text":
      return String(result);
    case "hl7":
      throw new TypeError("HL7 output needs a Message (or a string); set `output: json` to send objects as JSON");
    case "xml":
      throw new TypeError("XML output needs an XmlElement (or a string)");
  }
}
