// Generates the editor typings for `@ductus/sdk` from the HL7 schema.
// Hovering msg.PID[3] in VS Code shows "PID-3 Patient Identifier List (CX)".

import { DATATYPES, type FieldDef, isComposite, SEGMENT_NAMES, SEGMENTS } from "./schema.ts";

/** Common segments without field definitions yet: typed as generic segments. */
const GENERIC_SEGMENTS = [
  "PD1", "PV2", "IN1", "IN2", "IN3", "GT1", "ROL", "SPM", "TQ1", "TQ2", "TXA", "SCH", "AIS", "AIG", "AIL",
  "AIP", "RGS", "FT1", "PR1", "IAM", "DB1", "ACC", "UB1", "UB2", "QRD", "QRF", "RXA", "RXR", "RXE", "RXO",
  "RXC", "RXD", "RXG", "CTI", "BLG", "ZPD",
];

function doc(text: string, indent: string): string {
  return `${indent}/** ${text.replace(/\*\//g, "*\\/")} */\n`;
}

function datatypeInterface(name: string, components: readonly FieldDef[]): string {
  let out = `  /** ${name} data type */\n  export interface ${name} extends Field {\n`;
  components.forEach(([label, type], i) => {
    out += doc(`${name}.${i + 1} ${label} (${type})`, "    ");
    out += `    ${i + 1}: string;\n`;
  });
  return out + "  }\n";
}

function segmentInterface(name: string, fields: readonly FieldDef[]): string {
  const title = SEGMENT_NAMES[name] ?? name;
  let out = `  /** ${name}: ${title} */\n  export interface ${name} extends Segment {\n`;
  fields.forEach(([label, type], i) => {
    const n = i + 1;
    const text = `${name}-${n} ${label} (${type})`;
    if (!isComposite(name, n)) {
      out += doc(text, "    ") + `    ${n}: string;\n`;
    } else {
      const t = type in DATATYPES ? type : "Field";
      out += doc(text, "    ") + `    get ${n}(): ${t};\n`;
      out += doc(text, "    ") + `    set ${n}(value: ${t} | string);\n`;
    }
  });
  return out + "  }\n";
}

const BASE = `
  /** A composite HL7 field, or one repetition of it. Index for components: \`field[1]\`. */
  export interface Field extends Iterable<Field> {
    /** Text of the (first) repetition, separators included. */
    readonly value: string;
    /** Number of repetitions. */
    readonly count: number;
    /** Repetition n (1-based). */
    rep(n: number): Field;
    /** Whole field text, all repetitions. */
    toString(): string;
    [component: number]: string;
  }

  /** One occurrence of a segment. Index for fields: \`seg[3]\`. */
  export interface Segment {
    readonly name: string;
    /** Field n as a Field, even for primitive types (for repetitions or raw access). */
    field(n: number): Field;
    toString(): string;
    [field: number]: any;
  }

  /** All segments with one name. Indexing reads the first; iterating visits each. */
  export type Segments<S extends Segment = Segment> = S & Iterable<S> & {
    /** How many of these segments the message has. */
    readonly count: number;
    readonly exists: boolean;
    /** Occurrence n (1-based). */
    rep(n: number): S;
    /** Remove every segment with this name. */
    remove(): void;
  };

  /** Parse raw HL7 text. */
  export function parse(raw: string): Message;

  /** Signature of an interface's \`transform\` export (may be async). Return null or kill() to drop the message. */
  export type Transform = (msg: Message) => Message | string | null | Kill | Promise<Message | string | null | Kill>;

  /** A deliberate drop, with a reason. Create with \`kill()\`. */
  export class Kill {
    readonly reason: string;
  }
  /**
   * Drop this message: it's never delivered, and it's kept (status "filtered") with the reason for audit.
   * \`return null\` does the same without a reason.
   */
  export function kill(reason?: string): Kill;
  /** Error that sends the message to quarantine with a clear reason. */
  export class QuarantineError extends Error {}
  /** Send this message to quarantine for review: \`throw quarantine("missing MRN")\`. */
  export function quarantine(reason: string): QuarantineError;

  type Param = string | number | boolean | null;
  type Row = Record<string, unknown>;
  /** A database defined in resources.yaml. */
  export interface Database {
    /** Rows as objects. Use $1, $2... placeholders; cast in SQL if needed ($1::int). */
    query<T = Row>(sql: string, params?: Param[]): Promise<T[]>;
    /** The first row, or null. */
    one<T = Row>(sql: string, params?: Param[]): Promise<T | null>;
    /** Run a statement; returns rows affected. */
    execute(sql: string, params?: Param[]): Promise<number>;
  }
  /** A database defined in resources.yaml, by name. */
  export function db(name: string): Database;
  /** A variable from environments/<env>.yaml. Throws if it isn't defined. */
  export function env(name: string): string;
  /** A secret from the environment's secret store. Throws if it isn't defined. */
  export function secret(name: string): string;

  /** Register a test (run with \`ductus test\`). */
  export function test(name: string, fn: () => void): void;
  /** Assert on a value. HL7 fields and segments are compared by their text. */
  export function expect(actual: unknown): Expectation;
  export interface Expectation {
    readonly not: Expectation;
    toBe(expected: unknown): void;
    toEqual(expected: unknown): void;
    toBeNull(): void;
    toBeTruthy(): void;
    toContain(item: unknown): void;
    toThrow(messagePart?: string): void;
  }
  /** Contents of a file in the interface's \`fixtures/\` folder. */
  export function fixture(name: string): string;
  /** A fixture parsed as an HL7 message. */
  export function hl7(name: string): Message;
`;

/** Full contents of the generated `ductus-sdk.d.ts`. */
export function generateTypes(): string {
  let out = "// Generated by `ductus types`. Do not edit.\n";
  out += `declare module "@ductus/sdk" {\n${BASE}\n`;

  for (const [name, components] of Object.entries(DATATYPES)) out += datatypeInterface(name, components);
  out += "\n";
  for (const [name, fields] of Object.entries(SEGMENTS)) out += segmentInterface(name, fields);

  out += "\n  /** A parsed HL7 message. Segments by name (msg.PID), plus helpers. */\n  export interface Message {\n";
  for (const name of Object.keys(SEGMENTS)) {
    out += doc(`${name}: ${SEGMENT_NAMES[name] ?? name}`, "    ") + `    readonly ${name}: Segments<${name}>;\n`;
  }
  for (const name of GENERIC_SEGMENTS) out += `    readonly ${name}: Segments;\n`;
  out += `    /** Custom Z-segments. */
    readonly [segment: \`Z\${string}\`]: Segments;
    /** Value at an HL7 path like "PID-3.1" (first repetition). Missing reads as "". */
    get(path: string): string;
    /** Set the value at an HL7 path like "PID-5.1". */
    set(path: string, value: string): void;
    /** Append a segment. Field values are raw HL7, index 0 = field 1. */
    add(name: string, fields?: readonly string[]): Segment;
    /** Insert a segment after the last segment named \`after\`. */
    insertAfter(after: string, name: string, fields?: readonly string[]): Segment;
    /** Remove every segment with this name. */
    remove(name: string): void;
    /** Segment names in order, e.g. ["MSH", "EVN", "PID"]. */
    readonly segmentNames: string[];
    /** The message as HL7 text. */
    toString(): string;
  }
}
`;
  return out;
}
