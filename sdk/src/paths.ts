// `getPath`: one reader for a decoded message of any format, with the paths keys take in
// interface.yaml, so a key tested in code is the key the engine orders by.

import { closingBracket, filterValue, splitOutside, XmlElement } from "./formats.ts";
import { isMessage } from "./hl7.ts";

type JsonStep = { key: string } | { index: number } | { filters: { path: JsonStep[]; value: string }[] };

/**
 * A JSON path, the syntax keys take (`crates/ductus-core/src/keys.rs`, checked against it by
 * `sdk/tests/key-paths.json`): names between dots, `[n]` from 0, `[name=value]` filters (the name
 * may be a path itself, `|` between alternatives, a value with `.`, `|` or `]` quoted).
 */
function jsonPath(path: string): JsonStep[] {
  const fail = (why: string): never => {
    throw new Error(
      `invalid JSON path "${path}": ${why} (e.g. "patient.mrn", "ids[0].value", "identifier[system=urn:oid:1.2.3].value")`,
    );
  };
  const steps = (text: string): JsonStep[] => {
    const out: JsonStep[] = [];
    for (const part of splitOutside(text, ".")) {
      const open = part.indexOf("[");
      const nameEnd = open < 0 ? part.length : open;
      const name = part.slice(0, nameEnd);
      if (/[\]=|'"]/.test(name)) fail(`\`${name}\` isn't a name`);
      if (name !== "") out.push({ key: name });
      else if (nameEnd === part.length) fail("an empty name between dots");
      let rest = part.slice(nameEnd);
      while (rest.startsWith("[")) {
        const inside = rest.slice(1);
        const end = closingBracket(inside);
        if (end < 0) fail("a `[` without its `]`");
        const bracket = inside.slice(0, end);
        if (/^[0-9]*$/.test(bracket)) {
          if (bracket === "") fail("an empty `[]`");
          out.push({ index: Number(bracket) });
        } else {
          out.push({
            filters: splitOutside(bracket, "|").map((alt) => {
              const [on, ...value] = splitOutside(alt, "=");
              if (value.length === 0) fail(`a filter is \`name=value\`: \`${alt}\``);
              return { path: steps(on), value: filterValue(value.join("="), fail) };
            }),
          });
        }
        rest = inside.slice(end + 1);
      }
      if (rest !== "") fail(`\`${rest}\` after \`]\``);
    }
    return out;
  };
  return steps(path);
}

function jsonAt(value: unknown, steps: JsonStep[]): unknown {
  let current = value;
  for (const step of steps) {
    const isObject = typeof current === "object" && current !== null && !Array.isArray(current);
    if ("key" in step) {
      if (!isObject || !Object.hasOwn(current as object, step.key)) return undefined;
      current = (current as Record<string, unknown>)[step.key];
    } else if ("index" in step) {
      if (!Array.isArray(current)) return undefined;
      current = current[step.index];
    } else {
      // In an array, the first item the first filter matches, else the second filter's; on an
      // object, the object itself if a filter matches.
      const hit = (item: unknown, f: { path: JsonStep[]; value: string }) => jsonText(jsonAt(item, f.path)) === f.value;
      const here = current;
      if (Array.isArray(here)) {
        current = undefined;
        for (const f of step.filters) {
          current = here.find((item) => hit(item, f));
          if (current !== undefined) break;
        }
      } else if (isObject) current = step.filters.some((f) => hit(here, f)) ? here : undefined;
      else return undefined;
    }
    if (current === undefined) return undefined;
  }
  return current;
}

/** A value as a key: text as it is, nothing or `null` as "", anything else as JSON. */
function jsonText(value: unknown): string {
  if (value === undefined || value === null) return "";
  if (typeof value === "string") return value;
  return typeof value === "object" ? JSON.stringify(value) : String(value);
}

/**
 * The value at a path in a decoded message, as text ("" when missing), with the paths
 * `ordering.key` and `dedupe.keys` take, so a key read here is the key the engine orders by:
 *
 *   getPath(msg, "PID-3[.5=MR].1")                          // HL7: msg.get
 *   getPath(doc, "id[@root=2.16.840.1]/@extension")         // XML: doc.get
 *   getPath(patient, "identifier[system=urn:oid:1.2.3].value") // JSON (any parsed value)
 *
 * An invalid path throws, saying why.
 */
export function getPath(message: unknown, path: string): string {
  if (isMessage(message)) return message.get(path);
  if (message instanceof XmlElement) return message.get(path);
  return jsonText(jsonAt(message, jsonPath(path)));
}
