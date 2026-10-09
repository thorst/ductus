import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import { byTag, curl, example, flatten, roleOf, typeText, type Document } from "./openapi";

// The engine's own document (`ductus api-spec`, checked against the binary by check.sh).
const doc = JSON.parse(readFileSync(resolve(__dirname, "../../openapi.json"), "utf8")) as Document;
const all = byTag(doc).flatMap((g) => g.endpoints);
const find = (method: string, path: string) => all.find((e) => e.method === method && e.path === path)!;

describe("the API reference", () => {
  it("lists every endpoint once, by tag in the document's order", () => {
    const count = Object.values(doc.paths).reduce((n, item) => n + Object.keys(item).length, 0);
    expect(all).toHaveLength(count);
    expect(byTag(doc)[0]?.tag).toBe(doc.tags?.[0]?.name);
    expect(new Set(all.map((e) => e.id)).size).toBe(all.length);
  });

  it("knows who may call what", () => {
    expect(find("get", "/health").role).toBe("none");
    expect(find("get", "/interfaces").role).toBe("viewer");
    expect(find("post", "/drain").role).toBe("operator");
    expect(find("post", "/tokens").role).toBe("admin");
    expect(roleOf({ summary: "Delete it. Admin.", responses: {} })).toBe("admin");
    expect(roleOf({ summary: "Ask the Operator. Then go.", responses: {} })).toBe("viewer");
  });

  it("writes types as people read them", () => {
    expect(typeText({ type: ["string", "null"], format: "date-time" })).toBe("string (date-time) or null");
    expect(typeText({ type: "array", items: { $ref: "#/components/schemas/Check" } })).toBe("array of Check");
    expect(typeText({ enum: ["ok", "warn"] })).toBe('"ok" | "warn"');
    expect(typeText({ oneOf: [{ type: "null" }, { $ref: "#/components/schemas/PauseInfo" }] })).toBe(
      "null or PauseInfo",
    );
    expect(typeText({ type: "integer", format: "int64" })).toBe("integer (int64)");
  });

  it("merges allOf parts into one property list", () => {
    const detail = flatten(doc, { $ref: "#/components/schemas/InterfaceDetail" });
    expect(detail.properties).toHaveProperty("name"); // from InterfaceStatus
    expect(detail.properties).toHaveProperty("folder"); // its own
    expect(detail.required).toContain("folder");
  });

  it("gives a curl command that runs as is", () => {
    expect(curl(doc, find("get", "/health"), "http://127.0.0.1:7700/api/v1")).toBe(
      "curl -s 'http://127.0.0.1:7700/api/v1/health'",
    );
    const resend = curl(doc, find("post", "/messages/{id}/resend"), "http://h/api/v1");
    expect(resend).toContain("'http://h/api/v1/messages/ID/resend'"); // braces would be a curl glob
    expect(resend).toContain('-H "Authorization: Bearer $DUCTUS_TOKEN"');
    expect(resend).toContain("-X POST");
    // Required fields only: what the engine accepts as is ("resend what's quarantined").
    expect(resend).toMatch(/-d '\{\}'$/);
    expect(curl(doc, find("post", "/messages/resend"), "http://h/api/v1")).toMatch(/-d '\{"ids":\[0\]\}'$/);
    const shape = { type: "object", required: ["it"], properties: { it: { type: "string" }, not: { type: "string" } } };
    expect(example(doc, shape)).toEqual({ it: "" });
  });
});
