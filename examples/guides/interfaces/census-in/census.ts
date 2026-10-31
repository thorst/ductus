import { type CsvRecord, kill, type Meta, parseHl7 } from "@ductus/sdk";

/** One census row (mrn, last, first, unit, room, bed) as an ADT A08 that updates the location. */
export function transform(row: CsvRecord, meta: Meta) {
  if (!row.mrn) return kill("a row without an MRN");
  const now = hl7Time(new Date(meta.receivedAt));
  const msg = parseHl7(`MSH|^~\\&|BEDBOARD|HOSP|EHR|HOSP|${now}||ADT^A08^ADT_A01|BB${meta.id}|P|2.5`);
  msg.add("EVN", ["A08", now]);
  msg.add("PID");
  msg.set("PID-3.1", row.mrn);
  msg.set("PID-5.1", row.last);
  msg.set("PID-5.2", row.first);
  msg.add("PV1");
  msg.set("PV1-2", "I");
  msg.set("PV1-3.1", row.unit);
  msg.set("PV1-3.2", row.room);
  msg.set("PV1-3.3", row.bed);
  return msg;
}

/** A time as an HL7 timestamp in UTC (`20261031133000+0000`). */
function hl7Time(t: Date): string {
  return t.toISOString().slice(0, 19).replace(/[-T:]/g, "") + "+0000";
}
