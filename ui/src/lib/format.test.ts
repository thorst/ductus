import { describe, expect, it } from "vitest";
import { ago, bytes, count, duration, ms, perMinute } from "./format";

describe("format", () => {
  it("says spans of time the way the engine does", () => {
    expect(duration(0)).toBe("0 s");
    expect(duration(59.4)).toBe("59 s");
    expect(duration(12 * 60 + 30)).toBe("12 min");
    expect(duration(3 * 3600)).toBe("3 h");
    expect(duration(3 * 3600 + 5 * 60)).toBe("3 h 5 min");
    expect(duration(2 * 86400 + 4 * 3600)).toBe("2 d 4 h");
    expect(duration(null)).toBe("–");
  });

  it("says how long ago", () => {
    const now = Date.parse("2026-10-09T12:00:00Z");
    expect(ago("2026-10-09T11:59:58Z", now)).toBe("just now");
    expect(ago("2026-10-09T11:48:00Z", now)).toBe("12 min ago");
    expect(ago(null, now)).toBe("never");
  });

  it("formats counts, sizes, times and rates", () => {
    expect(count(1240)).toBe("1,240");
    expect(count(undefined)).toBe("–");
    expect(bytes(512)).toBe("512 B");
    expect(bytes(2048)).toBe("2.0 KB");
    expect(ms(1.23)).toBe("1.2 ms");
    expect(ms(250.4)).toBe("250 ms");
    expect(ms(1500)).toBe("1.50 s");
    expect(perMinute(4.25)).toBe("4.3/min");
    expect(perMinute(400)).toBe("400/min");
  });
});
