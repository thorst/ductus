import { expect, hl7, type Meta, mock, test } from "@ductus/sdk";
import { transform } from "./billing.ts";

const meta: Meta = { id: 41, receivedAt: "2026-10-31T13:30:00Z", destination: "default", origin: 7 };

test("an admission becomes an encounter, with the attending's NPI", async () => {
  const calls = mock.db("provider_db", () => [{ npi: "1093817465" }]);
  const out = await transform(hl7("a01.hl7"), meta);
  expect(out).toEqual({
    patient: { mrn: "123456", family: "SMITH", given: "ANNA", birthDate: "19800214" },
    visit: { number: "V9001", unit: "4W", admitted: "20261031083000-0500" },
    event: "admit",
    attendingNpi: "1093817465",
    sourceMessage: 41,
  });
  expect(calls[0].params).toEqual(["1234"]);
});

test("the NPI is looked up once an hour, not once a message", async () => {
  const calls = mock.db("provider_db", () => [{ npi: "1588667638" }]);
  for (const file of ["a01.hl7", "a03.hl7"]) {
    const msg = hl7(file);
    msg.set("PV1-7.1", "5678");
    await transform(msg, meta);
  }
  expect(calls.length).toBe(1);
});

test("a provider the database doesn't know is quarantined", async () => {
  mock.db("provider_db", () => []);
  const msg = hl7("a01.hl7");
  msg.set("PV1-7.1", "9999");
  await expect(transform(msg, meta)).toReject("provider 9999 has no NPI");
});
