import { expect, fixture, Kill, type Message, type Meta, parseCsv, test } from "@ductus/sdk";
import { transform } from "./census.ts";

const meta: Meta = { id: 501, receivedAt: "2026-10-31T13:30:00Z", destination: "default" };

// With `split: records` the engine gives `transform` one row at a time, and so does `input()`:
// the fixture's first row. For a file of several rows, parse it and pick one. Fixtures can be
// read only inside a test, so this is a function.
const row = (n: number) => parseCsv(fixture("census_20261031.csv"))[n];

test("a row becomes an A08 with the patient's bed", () => {
  const msg = transform(row(0), meta) as Message;
  expect(msg.toString()).toBe(
    "MSH|^~\\&|BEDBOARD|HOSP|EHR|HOSP|20261031133000+0000||ADT^A08^ADT_A01|BB501|P|2.5\r" +
      "EVN|A08|20261031133000+0000\r" +
      "PID|||123456||SMITH^ANNA\r" +
      "PV1||I|4W^401^A\r",
  );
});

test("a row without an MRN is dropped with a reason", () => {
  const out = transform(row(1), meta);
  expect(out instanceof Kill && out.reason).toBe("a row without an MRN");
});
