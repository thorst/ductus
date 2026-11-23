// SDK tests: run inside the real engine by `cargo test -p ductus-script`.
// The HL7 v2.5 definitions (sdk/src/schema.ts): every type they name exists, each segment has
// v2.5's field count, and sample feeds of each kind read by name at the positions HL7 puts them.
import { __generateTypes, __schema, expect, type Message, parse, test } from "@ductus/sdk";

const { DATATYPES, GENERIC_SEGMENTS, PRIMITIVES, SEGMENT_NAMES, SEGMENTS } = __schema;

/** Field number of `label` in `segment`'s definition, so a test reads by the standard's name. */
function at(segment: string, label: string): number {
  const i = SEGMENTS[segment]?.findIndex(([name]) => name === label) ?? -1;
  if (i < 0) throw new Error(`${segment} has no field named "${label}"`);
  return i + 1;
}

const MSH = (type: string) => `MSH|^~\\&|SENDER|HOSP|DUCTUS|HOSP|20261122083000||${type}|CTRL-1|P|2.5\r`;

const SAMPLES: Record<string, string> = {
  // Registration with insurance and guarantor.
  "ADT^A04": MSH("ADT^A04^ADT_A01") +
    "EVN|A04|20261122083000\r" +
    "PID|1||445566^^^HOSP^MR||DOE^JANE^A||19750312|F|||22 OAK AVE^^YORK^PA^17401\r" +
    "PD1|||YORK FAMILY PRACTICE^^1234|7788^WELBY^MARCUS^^^DR\r" +
    "ROL|1|AD|PP^Primary Care Provider^HL70443|7788^WELBY^MARCUS\r" +
    "PV1|1|O|CLINIC^^^HOSP\r" +
    "PV2|||CHK^Annual checkup\r" +
    "GT1|1||DOE^JANE^A||22 OAK AVE^^YORK^PA^17401|^PRN^PH^^^717^5550100|||||SEL^Self\r" +
    "IN1|1|BCBS-PPO^Blue Cross PPO|BC001|BLUE CROSS OF PA|PO BOX 100^^HARRISBURG^PA^17101||^WPN^PH^^^800^5550199|GRP-42||||||||DOE^JANE^A|SEL^Self|19750312\r" +
    "IN2|1234567^^^EMP|123456789\r",
  // Charges.
  "DFT^P03": MSH("DFT^P03^DFT_P03") +
    "EVN|P03|20261122083000\r" +
    "PID|1||445566^^^HOSP^MR||DOE^JANE\r" +
    "PV1|1|O\r" +
    "FT1|1|TX-9001||20261121|20261122|CG|99213^Office visit est^CPT4|||1\r" +
    "PR1|1|CPT4|99213^Office visit est^CPT4||20261121\r",
  // A transcribed document.
  "MDM^T02": MSH("MDM^T02^MDM_T02") +
    "EVN|T02|20261122090000\r" +
    "PID|1||445566^^^HOSP^MR||DOE^JANE\r" +
    "PV1|1|I\r" +
    "TXA|1|DS|TX|20261122090000|7788^WELBY^MARCUS|||||||DOC-777^HOSP|||||AU\r" +
    "OBX|1|TX|DISCHARGE^Discharge summary||Discharged home in good condition.||||||F\r",
  // A booked appointment.
  "SIU^S12": MSH("SIU^S12^SIU_S12") +
    "SCH|APT-100^SCHED|APT-100^SCHED|||||ROUTINE^Routine visit|FOLLOWUP^Follow up|30|MIN^minutes\r" +
    "TQ1|1||||||20261201093000|20261201100000\r" +
    "PID|1||445566^^^HOSP^MR||DOE^JANE\r" +
    "RGS|1|A\r" +
    "AIS|1|A|99213^Office visit^CPT4|20261201093000|||30|MIN\r" +
    "AIG|1|A|ROOM-EXAM^Exam room|EQUIP\r" +
    "AIL|1|A|CLINIC^EXAM1^^HOSP\r" +
    "AIP|1|A|7788^WELBY^MARCUS|D^Doctor\r",
  // A vaccination for the registry.
  "VXU^V04": MSH("VXU^V04^VXU_V04") +
    "PID|1||445566^^^HOSP^MR||DOE^JANE\r" +
    "ORC|RE||IZ-555^HOSP\r" +
    "RXA|0|1|20261120101500|20261120101500|141^Influenza, seasonal^CVX|0.5|mL^milliliters^UCUM||00^New immunization record^NIP001|7788^WELBY^MARCUS|||||LOT-A1|20270630|SKB^GlaxoSmithKline^MVX|||CP|A\r" +
    "RXR|C28161^Intramuscular^NCIT|LD^Left Deltoid^HL70163\r",
  // A pharmacy order.
  "RDE^O11": MSH("RDE^O11^RDE_O11") +
    "PID|1||445566^^^HOSP^MR||DOE^JANE\r" +
    "ORC|NW|RX-1001^HOSP\r" +
    "RXO|00069-3150-83^Amoxicillin 500mg cap^NDC|500||mg^milligram^UCUM\r" +
    "RXE|^^^20261122^^R|00069-3150-83^Amoxicillin 500mg cap^NDC|500||mg^milligram^UCUM|CAP^Capsule||||30|CAP|2|7788^WELBY^MARCUS\r" +
    "RXR|PO^Oral^HL70162\r",
  // A lab result with its specimen.
  "ORU^R01": MSH("ORU^R01^ORU_R01") +
    "PID|1||445566^^^HOSP^MR||DOE^JANE\r" +
    "OBR|1|ORD-1|FIL-1|GLU^Glucose\r" +
    "OBX|1|NM|GLU^Glucose||98|mg/dL\r" +
    "SPM|1|SP-88^HOSP||BLD^Blood^HL70487|||||||P^Patient^HL70369||||||20261122070000\r",
  // A practitioner master file update.
  "MFN^M02": MSH("MFN^M02^MFN_M02") +
    "MFI|PRA^Practitioner master file^HL70175||UPD|||NE\r" +
    "MFE|MAD|MFN-1|20261122|7788^WELBY^MARCUS|CWE\r",
};

const sample = (type: string): Message => parse(SAMPLES[type]);

test("every type a definition names is a data type or a primitive", () => {
  const known = (type: string) => type === "varies" || PRIMITIVES.includes(type) || type in DATATYPES;
  const unknown: string[] = [];
  for (const [owner, defs] of [...Object.entries(SEGMENTS), ...Object.entries(DATATYPES)]) {
    defs.forEach(([name, type], i) => {
      if (!known(type)) unknown.push(`${owner}.${i + 1} ${name}: ${type}`);
    });
  }
  expect(unknown).toEqual([]);
});

test("every defined segment has a title, and none is also typed generic", () => {
  expect(Object.keys(SEGMENTS).filter((s) => !SEGMENT_NAMES[s])).toEqual([]);
  expect(Object.keys(SEGMENT_NAMES).filter((s) => !SEGMENTS[s])).toEqual([]);
  expect(GENERIC_SEGMENTS.filter((s) => s in SEGMENTS)).toEqual([]);
});

test("each segment has HL7 v2.5's field count", () => {
  const V25: Record<string, number> = {
    MSH: 21, EVN: 7, PID: 39, NK1: 39, PV1: 52, ORC: 31, OBX: 19, NTE: 4, AL1: 6, DG1: 21, MRG: 7, MSA: 6,
    ERR: 12, PD1: 21, PV2: 49, ROL: 12, IN1: 53, IN2: 72, GT1: 57, PR1: 20, FT1: 31, TQ1: 14, SPM: 29, TXA: 23,
    SCH: 27, AIS: 12, AIG: 14, AIL: 12, AIP: 12, RXO: 28, RXE: 44, RXR: 6, RXA: 26, MFI: 6, MFE: 5,
  };
  const wrong = Object.entries(V25)
    .filter(([s, n]) => SEGMENTS[s].length !== n)
    .map(([s, n]) => `${s}: ${SEGMENTS[s].length}, v2.5 has ${n}`);
  expect(wrong).toEqual([]);
});

test("no sample segment has more fields than its definition", () => {
  const over: string[] = [];
  for (const [type, raw] of Object.entries(SAMPLES)) {
    for (const line of raw.split("\r").filter((l) => l && !l.startsWith("MSH"))) {
      const name = line.slice(0, 3);
      const fields = line.split("|").length - 1;
      if (SEGMENTS[name] && fields > SEGMENTS[name].length) over.push(`${type} ${name}: ${fields} fields`);
    }
  }
  expect(over).toEqual([]);
});

test("registration: PD1, ROL, PV2, GT1, IN1 and IN2 read by name", () => {
  const msg = sample("ADT^A04");
  expect(msg.PD1[at("PD1", "Patient Primary Care Provider Name & ID No.")][2]).toBe("WELBY");
  expect(msg.PD1[at("PD1", "Patient Primary Facility")][1]).toBe("YORK FAMILY PRACTICE");
  expect(msg.ROL[at("ROL", "Action Code")]).toBe("AD");
  expect(msg.ROL[at("ROL", "Role-ROL")][1]).toBe("PP");
  expect(msg.PV2[at("PV2", "Admit Reason")][1]).toBe("CHK");
  expect(msg.GT1[at("GT1", "Guarantor Name")][1]).toBe("DOE");
  expect(msg.GT1[at("GT1", "Guarantor Ph Num - Home")][7]).toBe("5550100");
  expect(msg.GT1[at("GT1", "Guarantor Relationship")][1]).toBe("SEL");
  expect(msg.IN1[at("IN1", "Insurance Plan ID")][1]).toBe("BCBS-PPO");
  expect(msg.IN1[at("IN1", "Insurance Company Name")][1]).toBe("BLUE CROSS OF PA");
  expect(msg.IN1[at("IN1", "Insurance Company Address")][3]).toBe("HARRISBURG");
  expect(msg.IN1[at("IN1", "Group Number")]).toBe("GRP-42");
  expect(msg.IN1[at("IN1", "Name Of Insured")][2]).toBe("JANE");
  expect(msg.IN1[at("IN1", "Insured's Relationship To Patient")][1]).toBe("SEL");
  expect(msg.IN1[at("IN1", "Insured's Date Of Birth")]).toBe("19750312");
  expect(msg.IN2[at("IN2", "Insured's Employee ID")][1]).toBe("1234567");
  expect(msg.IN2[at("IN2", "Insured's Social Security Number")]).toBe("123456789");
});

test("charges, documents and master files read by name", () => {
  const dft = sample("DFT^P03");
  expect(dft.FT1[at("FT1", "Transaction ID")]).toBe("TX-9001");
  expect(dft.FT1[at("FT1", "Transaction Type")]).toBe("CG");
  expect(dft.FT1[at("FT1", "Transaction Code")][1]).toBe("99213");
  expect(dft.FT1[at("FT1", "Transaction Quantity")]).toBe("1");
  expect(dft.PR1[at("PR1", "Procedure Code")][3]).toBe("CPT4");
  expect(dft.PR1[at("PR1", "Procedure Date/Time")]).toBe("20261121");

  const mdm = sample("MDM^T02");
  expect(mdm.TXA[at("TXA", "Document Type")]).toBe("DS");
  expect(mdm.TXA[at("TXA", "Primary Activity Provider Code/Name")][2]).toBe("WELBY");
  expect(mdm.TXA[at("TXA", "Unique Document Number")][1]).toBe("DOC-777");
  expect(mdm.TXA[at("TXA", "Document Completion Status")]).toBe("AU");

  const mfn = sample("MFN^M02");
  expect(mfn.MFI[at("MFI", "Master File Identifier")][1]).toBe("PRA");
  expect(mfn.MFI[at("MFI", "File-Level Event Code")]).toBe("UPD");
  expect(mfn.MFI[at("MFI", "Response Level Code")]).toBe("NE");
  expect(mfn.MFE[at("MFE", "Record-Level Event Code")]).toBe("MAD");
  expect(mfn.MFE[at("MFE", "Primary Key Value - MFE")][2]).toBe("WELBY");
  expect(mfn.MFE[at("MFE", "Primary Key Value Type")]).toBe("CWE");
});

test("scheduling reads by name", () => {
  const msg = sample("SIU^S12");
  expect(msg.SCH[at("SCH", "Filler Appointment ID")][1]).toBe("APT-100");
  expect(msg.SCH[at("SCH", "Appointment Reason")][1]).toBe("ROUTINE");
  expect(msg.SCH[at("SCH", "Appointment Type")][1]).toBe("FOLLOWUP");
  expect(msg.SCH[at("SCH", "Appointment Duration")]).toBe("30");
  expect(msg.TQ1[at("TQ1", "Start date/time")]).toBe("20261201093000");
  expect(msg.TQ1[at("TQ1", "End date/time")]).toBe("20261201100000");
  expect(msg.AIS[at("AIS", "Universal Service Identifier")][1]).toBe("99213");
  expect(msg.AIS[at("AIS", "Duration")]).toBe("30");
  expect(msg.AIG[at("AIG", "Resource ID")][1]).toBe("ROOM-EXAM");
  expect(msg.AIL[at("AIL", "Location Resource ID")][2]).toBe("EXAM1");
  expect(msg.AIP[at("AIP", "Personnel Resource ID")][2]).toBe("WELBY");
  expect(msg.AIP[at("AIP", "Resource Type")][1]).toBe("D");
  // RGS has no definition yet: a generic segment, every field a Field.
  expect(msg.RGS[2].value).toBe("A");
});

test("pharmacy, immunization and specimens read by name", () => {
  const vxu = sample("VXU^V04");
  expect(vxu.RXA[at("RXA", "Administered Code")][1]).toBe("141");
  expect(vxu.RXA[at("RXA", "Administered Amount")]).toBe("0.5");
  expect(vxu.RXA[at("RXA", "Administering Provider")][2]).toBe("WELBY");
  expect(vxu.RXA[at("RXA", "Substance Lot Number")]).toBe("LOT-A1");
  expect(vxu.RXA[at("RXA", "Substance Manufacturer Name")][1]).toBe("SKB");
  expect(vxu.RXA[at("RXA", "Completion Status")]).toBe("CP");
  expect(vxu.RXA[at("RXA", "Action Code - RXA")]).toBe("A");
  expect(vxu.RXR[at("RXR", "Administration Site")][1]).toBe("LD");

  const rde = sample("RDE^O11");
  expect(rde.RXO[at("RXO", "Requested Give Amount - Minimum")]).toBe("500");
  expect(rde.RXO[at("RXO", "Requested Give Units")][1]).toBe("mg");
  expect(rde.RXE[at("RXE", "Give Code")][1]).toBe("00069-3150-83");
  expect(rde.RXE[at("RXE", "Give Dosage Form")][1]).toBe("CAP");
  expect(rde.RXE[at("RXE", "Dispense Amount")]).toBe("30");
  expect(rde.RXE[at("RXE", "Number Of Refills")]).toBe("2");
  expect(rde.RXE[at("RXE", "Ordering Provider's DEA Number")][2]).toBe("WELBY");
  expect(rde.RXR[at("RXR", "Route")][1]).toBe("PO");

  const oru = sample("ORU^R01");
  expect(oru.SPM[at("SPM", "Specimen ID")][1]).toBe("SP-88");
  expect(oru.SPM[at("SPM", "Specimen Type")][1]).toBe("BLD");
  expect(oru.SPM[at("SPM", "Specimen Role")][1]).toBe("P");
  expect(oru.SPM[at("SPM", "Specimen Collection Date/Time")][1]).toBe("20261122070000");
});

test("a defined segment's simple fields are strings, its composite ones Fields", () => {
  const msg = sample("ADT^A04");
  expect(typeof msg.IN1[1]).toBe("string");
  expect(typeof msg.IN1[4]).toBe("object");
  expect(String(msg.IN1[4])).toBe("BLUE CROSS OF PA");
  msg.IN1[8] = "GRP|43";
  expect(msg.toString()).toContain("|GRP\\F\\43|");
});

test("the editor's typings name the new segments' fields", () => {
  const types = __generateTypes();
  expect(types).toContain("/** IN1: Insurance */");
  expect(types).toContain("/** IN1-4 Insurance Company Name (XON) */\n    get 4(): XON;");
  expect(types).toContain("/** IN1-8 Group Number (ST) */\n    8: string;");
  expect(types).toContain("readonly RXA: Segments<RXA>;");
  expect(types).toContain("/** LA2 data type */");
  expect(types).not.toContain("readonly PV2: Segments;");
  expect(types).toContain("readonly RGS: Segments;");
});
