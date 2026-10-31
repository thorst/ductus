import { expect, fixture, test, toCsv } from "@ductus/sdk";
import { transform } from "./remits.ts";

test("one row per claim, with the patient and the payment's trace number", () => {
  const rows = transform(fixture("remit.835"));
  expect(rows.length).toBe(3);
  expect(rows[0]).toEqual({
    trace: "EFT20261030A",
    payer: "ACME HEALTH PLAN",
    claim: "V9001",
    status: "paid (primary)",
    charged: "1800.00",
    paid: "1320.40",
    patient_owes: "200.00",
    payer_claim: "PCN44001",
    patient: "SMITH, ANNA",
  });
  expect(rows[1].status).toBe("denied");
});

test("the CSV the billing office gets", () => {
  const csv = toCsv(transform(fixture("remit.835")));
  expect(csv.split("\n")[0]).toBe("trace,payer,claim,status,charged,paid,patient_owes,payer_claim,patient");
  expect(csv.split("\n")[3]).toBe('EFT20261030A,ACME HEALTH PLAN,V9003,paid (primary),250.00,200.00,50.00,PCN44003,"GARCIA, MARIA"');
});

test("separators come from the ISA segment, not assumed", () => {
  const pipes = fixture("remit.835").replaceAll("*", "|").replaceAll("~", "!");
  expect(transform(pipes)[2].claim).toBe("V9003");
});

test("a file that isn't X12 is quarantined", () => {
  expect(() => transform("MSH|^~\\&|...")).toThrow("not an X12 interchange");
});
