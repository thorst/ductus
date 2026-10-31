import { quarantine } from "@ductus/sdk";

// X12 (EDI) read as text: the ISA segment says which characters separate elements, components
// and segments, so read them from it rather than assuming `*`, `:` and `~`.

/** One X12 segment: its id (`CLP`) and its elements, 1-based like X12's own numbering
 *  (`seg.e(1)` is CLP01). Missing elements read as "". */
export class X12Segment {
  constructor(readonly id: string, private readonly elements: readonly string[]) {}

  /** Element n (1-based). */
  e(n: number): string {
    return this.elements[n - 1] ?? "";
  }
}

/** Split an X12 interchange into its segments, with the separators its ISA segment declares. */
export function parseX12(text: string): X12Segment[] {
  const data = text.replace(/^﻿/, "").trimStart();
  if (!data.startsWith("ISA") || data.length < 106) throw quarantine("not an X12 interchange: it must start with ISA");
  const element = data[3]; // the character right after "ISA"
  const segment = data[105]; // ISA is fixed-length: its terminator is the 106th character
  return data
    .split(segment)
    .map((s) => s.trim())
    .filter((s) => s.length > 0)
    .map((s) => {
      const [id, ...rest] = s.split(element);
      return new X12Segment(id, rest);
    });
}
