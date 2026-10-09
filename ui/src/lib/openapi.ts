// Reading the engine's OpenAPI document for the API reference page: the endpoints by tag, who may
// call each, types as text with links to the schemas, and a curl example. Only what utoipa
// writes is covered (OpenAPI 3.1: $ref, allOf, oneOf, enum, `type: [x, "null"]`).

export interface Schema {
  $ref?: string;
  type?: string | string[];
  format?: string;
  description?: string;
  properties?: Record<string, Schema>;
  required?: string[];
  items?: Schema;
  enum?: unknown[];
  oneOf?: Schema[];
  anyOf?: Schema[];
  allOf?: Schema[];
  additionalProperties?: boolean | Schema;
  minimum?: number;
}

export interface Parameter {
  name: string;
  in: string;
  description?: string;
  required?: boolean;
  schema?: Schema;
}

type Content = Record<string, { schema?: Schema }>;

export interface Operation {
  tags?: string[];
  summary?: string;
  description?: string;
  operationId?: string;
  parameters?: Parameter[];
  requestBody?: { content: Content; required?: boolean; description?: string };
  responses: Record<string, { description?: string; content?: Content }>;
  security?: Record<string, string[]>[];
}

export interface Document {
  info: { title: string; description?: string; version: string };
  tags?: { name: string; description?: string }[];
  paths: Record<string, Record<string, Operation>>;
  components?: { schemas?: Record<string, Schema> };
}

export const METHODS = ["get", "post", "put", "patch", "delete"] as const;
export type Method = (typeof METHODS)[number];

/** Who may call it: no token at all, any token (viewer and up), or at least this role. */
export type Role = "none" | "viewer" | "operator" | "admin";

export interface Endpoint {
  method: Method;
  path: string;
  op: Operation;
  /** For anchors: `op-post-messages-id-resend`. */
  id: string;
  role: Role;
}

/** The engine's handlers end their description with the role they need ("... Operator."). */
export function roleOf(op: Operation): Role {
  const [only, ...more] = op.security ?? [];
  if (only && more.length === 0 && Object.keys(only).length === 0) return "none";
  const text = (op.summary ?? "") + " " + (op.description ?? "");
  if (/\bAdmin\.\s*$/.test(text.trim())) return "admin";
  if (/\bOperator\.\s*$/.test(text.trim())) return "operator";
  return "viewer";
}

export const anchor = (method: string, path: string) =>
  `op-${method}-${path.replace(/[^a-zA-Z0-9]+/g, "-").replace(/^-|-$/g, "")}`;

/** Every endpoint, grouped by tag in the document's tag order (untagged last). */
export function byTag(doc: Document): { tag: string; description?: string; endpoints: Endpoint[] }[] {
  const groups = new Map<string, Endpoint[]>((doc.tags ?? []).map((t) => [t.name, []]));
  for (const [path, item] of Object.entries(doc.paths)) {
    for (const method of METHODS) {
      const op = item[method];
      if (!op) continue;
      const tag = op.tags?.[0] ?? "other";
      if (!groups.has(tag)) groups.set(tag, []);
      groups.get(tag)!.push({ method, path, op, id: anchor(method, path), role: roleOf(op) });
    }
  }
  return [...groups]
    .filter(([, endpoints]) => endpoints.length > 0)
    .map(([tag, endpoints]) => ({ tag, description: doc.tags?.find((t) => t.name === tag)?.description, endpoints }));
}

export const refName = (ref: string) => ref.slice(ref.lastIndexOf("/") + 1);

/** A schema with its `$ref` followed (once), and the name it had. */
export function resolve(doc: Document, schema: Schema): { name?: string; schema: Schema } {
  if (!schema.$ref) return { schema };
  const name = refName(schema.$ref);
  return { name, schema: doc.components?.schemas?.[name] ?? {} };
}

/** An object's properties with `allOf` parts merged (InterfaceDetail = InterfaceStatus + more). */
export function flatten(doc: Document, schema: Schema): { properties: Record<string, Schema>; required: string[] } {
  const s = resolve(doc, schema).schema;
  const out = { properties: { ...s.properties }, required: [...(s.required ?? [])] };
  for (const part of s.allOf ?? []) {
    const p = flatten(doc, part);
    Object.assign(out.properties, p.properties);
    out.required.push(...p.required);
  }
  return out;
}

/** A type as text and schema links: `["array of ", {ref: "Check"}]`, `["string or null"]`. */
export type TypeToken = string | { ref: string };

export function typeTokens(schema: Schema | undefined): TypeToken[] {
  if (!schema) return ["any"];
  if (schema.$ref) return [{ ref: refName(schema.$ref) }];
  const variants = schema.oneOf ?? schema.anyOf;
  if (variants) return join(variants.map(typeTokens), " or ");
  if (schema.allOf?.length === 1) return typeTokens(schema.allOf[0]);
  if (schema.enum) return [schema.enum.map((v) => JSON.stringify(v)).join(" | ")];
  const types = Array.isArray(schema.type) ? schema.type : schema.type ? [schema.type] : [];
  const nullable = types.includes("null");
  const base = types.filter((t) => t !== "null");
  if (nullable && base.length === 0) return ["null"];
  let tokens: TypeToken[];
  if (base.length === 0) tokens = [schema.properties || schema.allOf ? "object" : "any"];
  else if (base[0] === "array") tokens = ["array of ", ...typeTokens(schema.items)];
  else if (base[0] === "object" && typeof schema.additionalProperties === "object")
    tokens = ["map of ", ...typeTokens(schema.additionalProperties)];
  else tokens = [base.join(" or ") + (schema.format ? ` (${schema.format})` : "")];
  return nullable ? [...tokens, " or null"] : tokens;
}

function join(parts: TypeToken[][], separator: string): TypeToken[] {
  return parts.flatMap((p, n) => (n === 0 ? p : [separator, ...p]));
}

export const typeText = (schema: Schema | undefined) =>
  typeTokens(schema)
    .map((t) => (typeof t === "string" ? t : t.ref))
    .join("");

/**
 * A JSON value shaped like the schema, for request examples: required fields only, so the
 * example is a call the engine accepts (`{}` for "resend what's quarantined").
 */
export function example(doc: Document, schema: Schema | undefined, depth = 0): unknown {
  if (!schema || depth > 4) return null;
  const { schema: s } = resolve(doc, schema);
  if (s.enum) return s.enum[0];
  if (s.oneOf ?? s.anyOf) return example(doc, (s.oneOf ?? s.anyOf)![0], depth + 1);
  const type = (Array.isArray(s.type) ? s.type.find((t) => t !== "null") : s.type) ?? (s.properties ? "object" : "");
  if (s.allOf || type === "object") {
    const { properties, required } = flatten(doc, s);
    return Object.fromEntries(
      Object.entries(properties)
        .filter(([k]) => required.includes(k))
        .map(([k, v]) => [k, example(doc, v, depth + 1)]),
    );
  }
  if (type === "array") return [example(doc, s.items, depth + 1)];
  if (type === "integer" || type === "number") return 0;
  if (type === "boolean") return false;
  if (type === "string") return "";
  return null;
}

/** A curl command for the endpoint (path parameters as NAME, ID: braces are curl globs). */
export function curl(doc: Document, e: Endpoint, base: string): string {
  const path = e.path.replace(/\{(\w+)\}/g, (_, p: string) => p.toUpperCase());
  const lines = [`curl -s '${base}${path}'`];
  if (e.role !== "none") lines.push(`-H "Authorization: Bearer $DUCTUS_TOKEN"`);
  if (e.method !== "get") lines.push(`-X ${e.method.toUpperCase()}`);
  const body = e.op.requestBody?.content["application/json"]?.schema;
  if (body) {
    const json = JSON.stringify(example(doc, body)).replace(/'/g, `'"'"'`);
    lines.push(`-H 'Content-Type: application/json'`, `-d '${json}'`);
  }
  return lines.join(" \\\n  ");
}
