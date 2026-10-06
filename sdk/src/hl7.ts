// HL7v2 message model.
//
// Lazy: a message is split into segments once; a segment is split into fields only
// when touched, and untouched segments are written back out exactly as received.
//
// Rules:
//   - HL7 numbering everywhere: msg.PID[3][1] is PID-3.1.
//   - Indexing goes deeper, iterating goes across:
//       msg.OBX[5]                 -> OBX-5 of the first OBX
//       for (const obx of msg.OBX) -> every OBX
//       for (const id of msg.PID[3]) -> every repetition of PID-3
//   - Missing segments/fields read as "" and never throw.
//   - Primitive fields (ST, ID, NM, TS...) read as plain strings; composite fields
//     (CX, XPN, CE...) read as Field objects whose components are strings.

import { isComposite } from "./schema.ts";

export interface Delimiters {
  field: string;
  component: string;
  repetition: string;
  escape: string;
  subcomponent: string;
}

const SEGMENT_NAME = /^[A-Z][A-Z0-9]{2}$/;
const INDEX = /^[1-9][0-9]*$/;
const PATH = /^([A-Z][A-Z0-9]{2})-([1-9][0-9]*)(?:\.([1-9][0-9]*))?$/;

// ---------------------------------------------------------------------------
// Escaping

function decode(value: string, d: Delimiters): string {
  if (!value.includes(d.escape)) return value;
  const e = d.escape;
  return value
    .split(`${e}F${e}`).join(d.field)
    .split(`${e}S${e}`).join(d.component)
    .split(`${e}T${e}`).join(d.subcomponent)
    .split(`${e}R${e}`).join(d.repetition)
    .split(`${e}E${e}`).join(e);
}

function encode(value: string, d: Delimiters): string {
  const e = d.escape;
  let out = value.split(e).join(`${e}E${e}`);
  out = out.split(d.field).join(`${e}F${e}`);
  out = out.split(d.component).join(`${e}S${e}`);
  out = out.split(d.subcomponent).join(`${e}T${e}`);
  out = out.split(d.repetition).join(`${e}R${e}`);
  return out;
}

// ---------------------------------------------------------------------------
// Storage

class SegmentData {
  readonly name: string;
  private raw: string;
  private fields: string[] | null = null;
  private dirty = false;

  constructor(raw: string) {
    this.raw = raw;
    this.name = raw.slice(0, 3);
  }

  private split(d: Delimiters): string[] {
    if (this.fields === null) this.fields = this.raw.split(d.field);
    return this.fields;
  }

  /** MSH-1 is the field separator itself, so MSH field n sits at split index n-1. */
  private index(n: number): number {
    return this.name === "MSH" ? n - 1 : n;
  }

  getField(n: number, d: Delimiters): string {
    if (this.name === "MSH" && n === 1) return d.field;
    return this.split(d)[this.index(n)] ?? "";
  }

  setField(n: number, value: string, d: Delimiters): void {
    if (this.name === "MSH" && n <= 2) throw new Error("MSH-1 and MSH-2 (delimiters) are read-only");
    const fields = this.split(d);
    const i = this.index(n);
    while (fields.length <= i) fields.push("");
    fields[i] = value;
    this.dirty = true;
  }

  toString(d: Delimiters): string {
    return this.dirty && this.fields ? this.fields.join(d.field) : this.raw;
  }
}

// ---------------------------------------------------------------------------
// Views

function proxyIndexed<T extends object>(
  target: T,
  get: (n: number) => unknown,
  set: (n: number, value: unknown) => void,
): T {
  return new Proxy(target, {
    get(t, prop, receiver) {
      if (typeof prop === "string" && INDEX.test(prop)) return get(Number(prop));
      const value = Reflect.get(t, prop, t);
      return typeof value === "function" ? value.bind(t) : value;
    },
    set(t, prop, value) {
      if (typeof prop === "string" && INDEX.test(prop)) {
        set(Number(prop), value);
        return true;
      }
      return Reflect.set(t, prop, value, t);
    },
  });
}

/** A composite field (or one repetition of it). Index for components: field[1]. */
export class Field {
  /** `rep` 0 = the whole field (index reads the first repetition); n = repetition n. */
  constructor(
    private readonly msg: MessageImpl,
    private readonly seg: SegmentData | null,
    private readonly n: number,
    private readonly repIndex: number = 0,
  ) {
    return proxyIndexed(this, (c) => this.component(c), (c, v) => this.setComponent(c, String(v)));
  }

  private get d(): Delimiters {
    return this.msg.delimiters;
  }

  private repetitions(): string[] {
    const raw = this.seg ? this.seg.getField(this.n, this.d) : "";
    return raw === "" ? [] : raw.split(this.d.repetition);
  }

  private current(): string {
    return this.repetitions()[Math.max(this.repIndex, 1) - 1] ?? "";
  }

  /** Text of this repetition (the first one, for a whole field), separators included. */
  get value(): string {
    return this.current();
  }

  /** Number of repetitions. */
  get count(): number {
    return this.repetitions().length;
  }

  /** Repetition n (1-based). */
  rep(n: number): Field {
    return new Field(this.msg, this.seg, this.n, n);
  }

  private component(c: number): string {
    return decode(this.current().split(this.d.component)[c - 1] ?? "", this.d);
  }

  private setComponent(c: number, value: string): void {
    if (!this.seg) throw this.msg.missing();
    const reps = this.repetitions();
    const r = Math.max(this.repIndex, 1) - 1;
    while (reps.length <= r) reps.push("");
    const components = reps[r].split(this.d.component);
    while (components.length < c) components.push("");
    components[c - 1] = encode(value, this.d);
    reps[r] = components.join(this.d.component);
    this.seg.setField(this.n, reps.join(this.d.repetition), this.d);
  }

  *[Symbol.iterator](): Iterator<Field> {
    for (let i = 1; i <= this.count; i++) yield this.rep(i);
  }

  toString(): string {
    if (this.repIndex > 0) return this.current();
    return this.seg ? this.seg.getField(this.n, this.d) : "";
  }

  [Symbol.toPrimitive](): string {
    return this.toString();
  }
}

/** One occurrence of a segment. Index for fields: seg[3]. */
export class Segment {
  constructor(
    private readonly msg: MessageImpl,
    private readonly data: SegmentData | null,
    readonly name: string,
  ) {
    return proxyIndexed(this, (n) => this.read(n), (n, v) => this.write(n, v));
  }

  /** Always a Field, even for primitive types: for repetitions or raw access. */
  field(n: number): Field {
    return new Field(this.msg, this.data, n);
  }

  private read(n: number): string | Field {
    const raw = this.data ? this.data.getField(n, this.msg.delimiters) : "";
    if (this.name === "MSH" && n <= 2) return raw; // the delimiters themselves: never split or decode
    if (isComposite(this.name, n)) return this.field(n);
    return decode(raw.split(this.msg.delimiters.repetition)[0], this.msg.delimiters);
  }

  private write(n: number, value: unknown): void {
    if (!this.data) throw this.msg.missing(this.name);
    const d = this.msg.delimiters;
    // Composite values are written as raw HL7 (separators allowed); primitives are escaped.
    const text = value instanceof Field ? value.toString() : String(value ?? "");
    this.data.setField(n, isComposite(this.name, n) ? text : encode(text, d), d);
  }

  toString(): string {
    return this.data ? this.data.toString(this.msg.delimiters) : "";
  }
}

/** All segments with one name: msg.OBX. Indexing reads the first; iterating visits each. */
export class Segments {
  constructor(
    private readonly msg: MessageImpl,
    readonly name: string,
  ) {
    return proxyIndexed(
      this,
      (n) => (this.first() as unknown as Record<number, unknown>)[n],
      (n, v) => {
        (this.first() as unknown as Record<number, unknown>)[n] = v;
      },
    );
  }

  private all(): SegmentData[] {
    return this.msg.named(this.name);
  }

  private first(): Segment {
    return new Segment(this.msg, this.all()[0] ?? null, this.name);
  }

  get count(): number {
    return this.all().length;
  }

  get exists(): boolean {
    return this.count > 0;
  }

  /** Occurrence n (1-based). */
  rep(n: number): Segment {
    return new Segment(this.msg, this.all()[n - 1] ?? null, this.name);
  }

  field(n: number): Field {
    return this.first().field(n);
  }

  /** Remove every segment with this name. */
  remove(): void {
    this.msg.remove(this.name);
  }

  *[Symbol.iterator](): Iterator<Segment> {
    for (const s of this.all()) yield new Segment(this.msg, s, this.name);
  }

  toString(): string {
    return this.first().toString();
  }
}

// ---------------------------------------------------------------------------
// Message

class MessageImpl {
  readonly data: SegmentData[];
  /** Segments by name, built on first use and dropped whenever segments are added or removed. */
  private index: Map<string, SegmentData[]> | null = null;

  named(name: string): SegmentData[] {
    if (this.index === null) {
      this.index = new Map();
      for (const s of this.data) {
        const list = this.index.get(s.name);
        if (list) list.push(s);
        else this.index.set(s.name, [s]);
      }
    }
    return this.index.get(name) ?? [];
  }
  readonly delimiters: Delimiters;

  constructor(raw: string) {
    const text = raw.replace(/^[\r\n]+/, "");
    if (!text.startsWith("MSH") || text.length < 8) {
      throw new Error("not an HL7 message: it must start with an MSH segment");
    }
    this.delimiters = {
      field: text[3],
      component: text[4],
      repetition: text[5],
      escape: text[6],
      subcomponent: text[7],
    };
    this.data = text.split(/\r\n|\r|\n/).filter((s) => s.length > 0).map((s) => new SegmentData(s));
  }

  missing(name?: string): Error {
    return new Error(
      name ? `message has no ${name} segment; add it first with msg.add("${name}")` : "field belongs to a missing segment",
    );
  }

  get(path: string): string {
    const [segment, field, component] = parsePath(path);
    const seg = this.named(segment)[0];
    if (!seg) return "";
    const rep = seg.getField(field, this.delimiters).split(this.delimiters.repetition)[0];
    if (component === undefined) return rep;
    return decode(rep.split(this.delimiters.component)[component - 1] ?? "", this.delimiters);
  }

  set(path: string, value: string): void {
    const [segment, field, component] = parsePath(path);
    const s = new Segments(this, segment) as unknown as Record<number, unknown> & Segments;
    if (component === undefined) {
      s[field] = value;
    } else {
      (s.field(field) as unknown as Record<number, string>)[component] = value;
    }
  }

  add(name: string, fields: readonly string[] = []): Segment {
    return this.insertAt(this.data.length, name, fields);
  }

  insertAfter(after: string, name: string, fields: readonly string[] = []): Segment {
    let index = -1;
    this.data.forEach((s, i) => {
      if (s.name === after) index = i;
    });
    if (index < 0) throw this.missing(after);
    return this.insertAt(index + 1, name, fields);
  }

  private insertAt(index: number, name: string, fields: readonly string[]): Segment {
    if (!SEGMENT_NAME.test(name)) throw new Error(`invalid segment name "${name}"`);
    const raw = [name, ...fields].join(this.delimiters.field);
    const data = new SegmentData(raw);
    this.data.splice(index, 0, data);
    this.index = null;
    return new Segment(this, data, name);
  }

  remove(name: string): void {
    for (let i = this.data.length - 1; i >= 0; i--) {
      if (this.data[i].name === name) this.data.splice(i, 1);
    }
    this.index = null;
  }

  /** Segment names in order, e.g. ["MSH", "EVN", "PID", "PV1"]. */
  get segmentNames(): string[] {
    return this.data.map((s) => s.name);
  }

  toString(): string {
    return this.data.map((s) => s.toString(this.delimiters)).join("\r") + "\r";
  }
}

function parsePath(path: string): [string, number, number | undefined] {
  const m = PATH.exec(path);
  if (!m) throw new Error(`invalid HL7 path "${path}" (expected e.g. "PID-3" or "PID-3.1")`);
  return [m[1], Number(m[2]), m[3] === undefined ? undefined : Number(m[3])];
}

/** Parse raw HL7 into a Message. */
export function parse(raw: string): Message {
  const impl = new MessageImpl(raw);
  return new Proxy(impl, {
    get(t, prop) {
      if (typeof prop === "string" && SEGMENT_NAME.test(prop)) return new Segments(t, prop);
      const value = Reflect.get(t, prop, t);
      return typeof value === "function" ? value.bind(t) : value;
    },
  }) as unknown as Message;
}

/** A parsed HL7 message. Full typings come from the generated `@ductus/sdk` declarations. */
export type Message = MessageImpl & Record<string, Segments>;

export function isMessage(value: unknown): value is Message {
  return value instanceof MessageImpl;
}
