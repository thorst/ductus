import { expect, type HttpRequest, mock, test } from "@ductus/sdk";
import { transform } from "./lookup.ts";

const get = (mrn: string): HttpRequest =>
  ({ method: "GET", path: `/patients/${mrn}`, params: { mrn }, query: {}, headers: {}, text: "", body: "" });

test("a known MRN is answered with its enterprise id", async () => {
  const calls = mock.api("empi", { status: 200, body: { enterpriseId: "E-77821" } });
  const answer = await transform(get("123456"));
  expect(answer.status).toBe(200);
  expect(answer.body).toEqual({ mrn: "123456", enterpriseId: "E-77821" });
  expect(calls[0].url).toBe("/patients/123456");
});

test("an MRN the EMPI doesn't know is a 404", async () => {
  mock.api("empi", { status: 404 });
  const answer = await transform(get("999"));
  expect(answer.status).toBe(404);
  expect(answer.body).toEqual({ error: "no patient with MRN 999" });
});
