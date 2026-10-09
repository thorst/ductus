import { describe, expect, it } from "vitest";
import { fromEditable, isHl7, parseHl7, prettyJson, toEditable } from "./hl7";

const ADT = "MSH|^~\\&|EPIC|HOSP|LAB|HOSP|20261009||ADT^A01|C1|P|2.5\rPID|1||MRN1^^^HOSP||DOE^JANE\r";

describe("hl7 display", () => {
  it("numbers fields like the SDK, MSH-1 being the separator", () => {
    const [msh, pid] = parseHl7(ADT);
    expect(msh?.fields[0]).toMatchObject({ path: "MSH-1", value: "|" });
    expect(msh?.fields[1]).toMatchObject({ path: "MSH-2", value: "^~\\&", components: [] });
    expect(msh?.fields[8]).toMatchObject({ path: "MSH-9", value: "ADT^A01" });
    expect(msh?.fields[9]).toMatchObject({ path: "MSH-10", value: "C1" });
    expect(pid?.fields[2]?.components.map((c) => c.path)).toEqual(["PID-3.1", "PID-3.2", "PID-3.3", "PID-3.4"]);
    expect(pid?.fields[4]?.components[1]).toEqual({ path: "PID-5.2", value: "JANE" });
  });

  it("splits on any line ending and skips blank lines", () => {
    expect(parseHl7("MSH|^~\\&|A\r\nPID|1\n\nPV1|1").map((s) => s.name)).toEqual(["MSH", "PID", "PV1"]);
  });

  it("knows HL7 and JSON when it sees them", () => {
    expect(isHl7(ADT)).toBe(true);
    expect(isHl7('{"a":1}')).toBe(false);
    expect(prettyJson('{"a":[1,2]}')).toBe('{\n  "a": [\n    1,\n    2\n  ]\n}');
    expect(prettyJson("{nope")).toBeNull();
    expect(prettyJson("a,b,c")).toBeNull();
  });
});

describe("editing a message", () => {
  it("shows segments one per line and puts the separator back", () => {
    const text = toEditable(ADT);
    expect(text).toBe("MSH|^~\\&|EPIC|HOSP|LAB|HOSP|20261009||ADT^A01|C1|P|2.5\nPID|1||MRN1^^^HOSP||DOE^JANE\n");
    expect(fromEditable(text, true)).toBe(ADT);
    expect(fromEditable(text.replace("JANE", "JOAN") + "\n\n", true)).toBe(ADT.replace("JANE", "JOAN"));
    expect(fromEditable("a,b\r\nc,d\n", false)).toBe("a,b\r\nc,d\n");
  });
});
