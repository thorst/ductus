import { expect, mock, test } from "@ductus/sdk";
import { enterpriseId } from "../../lib/empi.ts";

test("the EMPI's id for an MRN", async () => {
  const calls = mock.api("empi", { status: 200, body: { enterpriseId: "E-77821" } });
  expect(await enterpriseId("123456")).toBe("E-77821");
  expect(calls[0].url).toBe("/patients/123456");
});

test("an MRN the EMPI doesn't know is quarantined", async () => {
  mock.api("empi", { status: 404 });
  await expect(enterpriseId("999")).toReject("the EMPI doesn't know MRN 999");
});
