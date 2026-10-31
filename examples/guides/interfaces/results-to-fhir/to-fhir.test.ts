import { expect, input, type Message, test } from "@ductus/sdk";
import { transform } from "./to-fhir.ts";

test("each OBX is an Observation, created only once per result", () => {
  const bundle = transform(input<Message>("oru-r01.hl7"));
  expect(bundle.type).toBe("transaction");
  expect(bundle.entry.length).toBe(3);
  expect(bundle.entry[0]).toEqual({
    resource: {
      resourceType: "Observation",
      identifier: [{ system: "http://hospital.org/lab-result", value: "L55012-1" }],
      status: "final",
      code: { coding: [{ system: "http://loinc.org", code: "2345-7", display: "Glucose" }] },
      subject: { reference: "Patient?identifier=http://hospital.org/mrn|123456" },
      effectiveDateTime: "2026-10-31T09:30:00-05:00",
      valueQuantity: { value: 95, unit: "mg/dL" },
    },
    request: {
      method: "POST",
      url: "Observation",
      ifNoneExist: "identifier=http://hospital.org/lab-result|L55012-1",
    },
  });
});

test("text results are strings, with their own status", () => {
  const comment: { valueString?: string; status: string } = transform(input<Message>("oru-r01.hl7")).entry[2].resource;
  expect(comment.valueString).toBe("Specimen slightly hemolyzed");
  expect(comment.status).toBe("preliminary");
});

test("a time without an offset is quarantined, not guessed", () => {
  const msg = input<Message>("oru-r01.hl7");
  msg.set("OBX-14", "20261031093000");
  expect(() => transform(msg)).toThrow(`OBX-14 "20261031093000" isn't a time with an offset`);
});
