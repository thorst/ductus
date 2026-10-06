// Service mocks, async tests and fetch(): run inside the real engine by `cargo test -p ductus-script`.
import { db, env, expect, fetch, hl7, type Message, mock, quarantine, secret, test } from "@ductus/sdk";

// A typical lookup interface: MRN → name from a database, then enrich from an API.
async function transform(msg: Message) {
  const mrn = msg.PID[3][1];
  const row = await db("patient_db").one<{ name: string }>("SELECT name FROM patients WHERE mrn = $1", [mrn]);
  if (!row) throw quarantine(`unknown MRN ${mrn}`);
  const r = await fetch(`${env("EMPI_URL")}/patients/${mrn}`, { headers: { Authorization: `Bearer ${secret("TOKEN")}` } });
  msg.add("ZLK", [row.name, r.json().enterpriseId]);
  return msg;
}

test("async tests await the transform; mocks answer db, fetch, env and secret", async () => {
  const queries = mock.db("patient_db", (_sql, [mrn]) => (mrn === "123456" ? [{ name: "SMITH" }] : []));
  const requests = mock.fetch({ body: { enterpriseId: "E-9" } });
  mock.env({ EMPI_URL: "https://empi.test" });
  mock.secret({ TOKEN: "t0k" });

  const out = await transform(hl7("adt.hl7"));
  expect(out.get("ZLK-1")).toBe("SMITH");
  expect(out.get("ZLK-2")).toBe("E-9");
  expect(queries).toEqual([{ sql: "SELECT name FROM patients WHERE mrn = $1", params: ["123456"] }]);
  expect(requests.length).toBe(1);
  expect(requests[0].url).toBe("https://empi.test/patients/123456");
  expect(requests[0].method).toBe("GET");
  expect(requests[0].headers.authorization).toBe("Bearer t0k");
});

test("toReject checks async failures", async () => {
  mock.db("patient_db", []);
  await expect(transform(hl7("adt.hl7"))).toReject("unknown MRN 123456");
  await expect(async () => 1).not.toReject();
  await expect(Promise.resolve(1)).toReject().then(
    () => { throw new Error("should have failed"); },
    (e: Error) => expect(e.message).toBe("expected it to reject"),
  );
});

test("mocks are reset before every test, and unmocked calls say how to mock them", async () => {
  await expect(db("patient_db").query("SELECT 1")).toReject('add mock.db("patient_db"');
  await expect(fetch("https://x.test/a")).toReject("fetch(GET https://x.test/a) is not mocked");
  expect(() => env("EMPI_URL")).toThrow('add mock.env({ EMPI_URL: "..." })');
  expect(() => secret("TOKEN")).toThrow("add mock.secret");
});

test("toThrow on an async function points to toReject", () => {
  expect(() => expect(async () => { throw new Error("x"); }).toThrow()).toThrow("use `await expect(...).toReject()`");
});

test("db mocks: execute counts, rows from a function", async () => {
  mock.db("billing", (sql) => (sql.startsWith("UPDATE") ? 3 : [{ n: 1 }, { n: 2 }]));
  expect(await db("billing").execute("UPDATE x SET y = 1")).toBe(3);
  expect(await db("billing").execute("INSERT ... RETURNING")).toBe(2);
  expect(await db("billing").one("SELECT n")).toEqual({ n: 1 });
  mock.db("billing", () => 5);
  await expect(db("billing").query("SELECT n")).toReject("return rows");
});

test("fetch: JSON bodies, POST by default with a body, query strings, response helpers", async () => {
  const calls = mock.fetch((req) => ({ status: 201, headers: { "X-Id": "7" }, body: JSON.parse(req.body ?? "null") }));
  const r = await fetch("https://api.test/orders?x=1", { body: { qty: 2 }, query: { "a b": "c&d" }, timeout: 500 });
  expect(calls[0].method).toBe("POST");
  expect(calls[0].url).toBe("https://api.test/orders?x=1&a%20b=c%26d");
  expect(calls[0].headers["content-type"]).toBe("application/json");
  expect(calls[0].timeoutMs).toBe(500);
  expect(r.status).toBe(201);
  expect(r.ok).toBe(true);
  expect(r.headers["x-id"]).toBe("7");
  expect(r.json()).toEqual({ qty: 2 });
  expect(r.text()).toBe('{"qty":2}');

  mock.fetch({ status: 200, body: "<html>" });
  const html = await fetch("https://api.test/page?key=SECRET");
  expect(() => html.json()).toThrow("GET https://api.test/page: response is not JSON: <html>");
  expect(() => html.json()).not.toThrow("SECRET");
});

test("fetch: 5xx, 408 and 429 retry the message unless retry: false; 4xx are responses", async () => {
  for (const status of [500, 503, 408, 429]) {
    mock.fetch({ status, body: "busy" });
    await expect(fetch("https://api.test/x?key=SECRET")).toReject(`[ductus:transient] GET https://api.test/x returned ${status}: busy`);
    expect((await fetch("https://api.test/x", { retry: false })).status).toBe(status);
  }
  mock.fetch({ status: 404 });
  const missing = await fetch("https://api.test/x");
  expect(missing.ok).toBe(false);
  expect(missing.status).toBe(404);
});
