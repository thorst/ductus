import { describe, expect, it } from "vitest";
import { countdown, needingRenewal } from "./certificates";

describe("a certificate's countdown", () => {
  it("says how long is left, in words", () => {
    expect(countdown({ state: "ok", days_left: 90 })).toEqual({ text: "90 days left", tone: "ok" });
    expect(countdown({ state: "expiring", days_left: 14 })).toEqual({ text: "14 days left", tone: "warn" });
    expect(countdown({ state: "expiring", days_left: 1 }).text).toBe("1 day left");
    expect(countdown({ state: "expiring", days_left: 0 }).text).toBe("Expires today");
  });

  it("says when it expired, and when it was never read", () => {
    expect(countdown({ state: "expired", days_left: -2 })).toEqual({ text: "Expired 2 days ago", tone: "bad" });
    expect(countdown({ state: "expired", days_left: -1 }).text).toBe("Expired 1 day ago");
    expect(countdown({ state: "expired", days_left: 0 }).text).toBe("Expired today");
    expect(countdown({ state: "unknown", days_left: null })).toEqual({ text: "Not read yet", tone: "unknown" });
  });

  it("lists the ones needing renewal, worst first", () => {
    const all = [
      { state: "ok", days_left: 90 },
      { state: "expiring", days_left: 20 },
      { state: "expired", days_left: -3 },
      { state: "unknown", days_left: null },
      { state: "expiring", days_left: 3 },
    ];
    expect(needingRenewal(all).map((c) => c.days_left)).toEqual([-3, 3, 20]);
  });
});
