import { describe, expect, it } from "vitest";
import type { Schemas } from "@/api/client";
import { myInterfaces, ownerGroup, unreachable } from "./owner";

const group = (owner: string, reach: string, you: boolean, owns: Schemas["Owned"][]): Schemas["OwnerGroup"] => ({
  owner,
  reach,
  you,
  owns,
  person: null,
  resources: [],
});

const groups = [
  group("Olaf Night", "user", true, [{ interface: "adt" }, { interface: "orders", destination: "lab" }]),
  group("Lab Systems", "team", false, [{ interface: "orders" }]),
  group("Gone Person", "disabled", false, [{ interface: "billing" }]),
];

describe("owners", () => {
  it("finds an interface's owner, and a destination's own owner", () => {
    expect(ownerGroup(groups, "orders")?.owner).toBe("Lab Systems");
    expect(ownerGroup(groups, "orders", "lab")?.owner).toBe("Olaf Night");
    expect(ownerGroup(groups, "nope")).toBeUndefined();
    expect(ownerGroup(undefined, "adt")).toBeUndefined();
  });

  it("counts an interface as mine when I own it or one of its destinations", () => {
    expect([...myInterfaces(groups)].sort()).toEqual(["adt", "orders"]);
    expect(myInterfaces(undefined).size).toBe(0);
  });

  it("flags owners that reach nobody or the wrong person", () => {
    expect(groups.map(unreachable)).toEqual([false, false, true]);
    expect(unreachable(group("x", "nobody", false, []))).toBe(true);
    expect(unreachable(group("x", "several", false, []))).toBe(true);
    expect(unreachable(group("x@h.org", "address", false, []))).toBe(false);
  });
});
