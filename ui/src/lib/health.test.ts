import { describe as suite, expect, it } from "vitest";
import type { Schemas } from "@/api/client";
import { problems, worst } from "./health";

type Interface = Schemas["InterfaceStatus"];
type Destination = Schemas["DestinationStatus"];

function destination(name: string, state: string, summary = state): Destination {
  return { name, health: { state, summary } } as Destination;
}

function iface(state: string, destinations: Destination[], stopped = false): Interface {
  return {
    name: "adt",
    state: stopped ? "stopped" : "running",
    stopped_reason: stopped ? "ran past its timeout 3 times" : null,
    health: { state, summary: state },
    destinations,
  } as Interface;
}

suite("health", () => {
  it("lists what isn't ok, worst first", () => {
    const i = iface("silent", [
      destination("lab", "ok"),
      destination("billing", "stalled"),
      destination("fax", "paused"),
    ]);
    expect(problems(i).map((p) => [p.destination, p.state])).toEqual([
      ["billing", "stalled"],
      [undefined, "silent"],
      ["fax", "paused"],
    ]);
    expect(worst(i)).toBe("stalled");
  });

  it("is ok when everything is", () => {
    expect(worst(iface("ok", [destination("lab", "ok")]))).toBe("ok");
  });

  it("a stopped interface is the worst there is", () => {
    const i = iface("ok", [destination("lab", "stalled")], true);
    expect(problems(i)[0]).toEqual({ state: "stopped", summary: "ran past its timeout 3 times" });
  });
});
