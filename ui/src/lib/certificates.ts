// How a certificate's countdown reads: words first (never color alone), then a tone for the icon.

export interface Countdown {
  state: string;
  days_left?: number | null;
}

export type Tone = "bad" | "warn" | "ok" | "unknown";

/** "Expired 2 days ago", "Expires today", "14 days left", "Not read yet", and its tone. */
export function countdown(c: Countdown): { text: string; tone: Tone } {
  const days = c.days_left;
  if (c.state === "unknown" || days == null) return { text: "Not read yet", tone: "unknown" };
  const plural = (n: number) => `${n} ${n === 1 ? "day" : "days"}`;
  if (c.state === "expired") {
    return { text: days >= 0 ? "Expired today" : `Expired ${plural(-days)} ago`, tone: "bad" };
  }
  const text = days === 0 ? "Expires today" : `${plural(days)} left`;
  return { text, tone: c.state === "expiring" ? "warn" : "ok" };
}

/** Certificates that need someone: expired or within the first step, worst first. */
export function needingRenewal<T extends Countdown>(all: T[]): T[] {
  return all
    .filter((c) => c.state === "expired" || c.state === "expiring")
    .sort((a, b) => (a.days_left ?? 0) - (b.days_left ?? 0));
}
