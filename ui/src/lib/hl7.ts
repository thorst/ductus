// HL7 v2 for display: segments, fields and components with the paths people use (PID-3.1),
// numbered like the SDK (1-based; MSH-1 is the field separator itself, MSH-2 the encoding
// characters). Display only: nothing here changes the message.

export interface Hl7Field {
  /** "PID-3" */
  path: string;
  value: string;
  /** Components when the field has more than one ("PID-3.1" ...), else none. */
  components: { path: string; value: string }[];
}

export interface Hl7Segment {
  name: string;
  fields: Hl7Field[];
}

/** True when the payload looks like HL7 v2 (starts with MSH and a field separator). */
export function isHl7(payload: string): boolean {
  return /^MSH./.test(payload.trimStart());
}

export function parseHl7(payload: string): Hl7Segment[] {
  const text = payload.trimStart();
  const fieldSep = text[3] ?? "|";
  const componentSep = text[4] ?? "^";
  return text
    .split(/\r\n|\r|\n/)
    .filter((line) => line.trim() !== "")
    .map((line) => {
      const name = line.slice(0, 3);
      const values = line.split(fieldSep).slice(1);
      // MSH-1 is the separator itself, which the split ate; MSH-2 the encoding characters (never split).
      if (name === "MSH") values.unshift(fieldSep);
      const fields = values.map((value, n) => {
        const path = `${name}-${n + 1}`;
        const parts = name === "MSH" && n < 2 ? [value] : value.split(componentSep);
        return {
          path,
          value,
          components: parts.length > 1 ? parts.map((v, c) => ({ path: `${path}.${c + 1}`, value: v })) : [],
        };
      });
      return { name, fields };
    });
}

/** Pretty JSON when the payload is JSON, else null. */
export function prettyJson(payload: string): string | null {
  const t = payload.trim();
  if (!(t.startsWith("{") || t.startsWith("["))) return null;
  try {
    return JSON.stringify(JSON.parse(t), null, 2);
  } catch {
    return null;
  }
}
