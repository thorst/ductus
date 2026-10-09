// Numbers and times the way the engine's own summaries say them ("12 min", "1,240").

const integer = new Intl.NumberFormat("en-US");

export function count(n: number | null | undefined): string {
  return n == null ? "–" : integer.format(n);
}

/** A span of seconds: "45 s", "12 min", "3 h 5 min", "2 d 4 h". */
export function duration(seconds: number | null | undefined): string {
  if (seconds == null) return "–";
  const s = Math.max(0, Math.round(seconds));
  if (s < 60) return `${s} s`;
  const min = Math.floor(s / 60);
  if (min < 60) return `${min} min`;
  const h = Math.floor(min / 60);
  if (h < 24) return min % 60 ? `${h} h ${min % 60} min` : `${h} h`;
  const d = Math.floor(h / 24);
  return h % 24 ? `${d} d ${h % 24} h` : `${d} d`;
}

/** How long ago: "just now", "12 min ago", "3 h ago". */
export function ago(iso: string | null | undefined, now: number = Date.now()): string {
  if (!iso) return "never";
  const s = (now - Date.parse(iso)) / 1000;
  if (s < 5) return "just now";
  return `${duration(s)} ago`;
}

/** A time to the second in the viewer's time zone: "2026-10-09 14:03:27". */
export function timestamp(iso: string | null | undefined): string {
  if (!iso) return "–";
  const d = new Date(iso);
  const pad = (n: number) => String(n).padStart(2, "0");
  return (
    `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ` +
    `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`
  );
}

export function bytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

/** Milliseconds as the engine reports them (one decimal below 10 ms). */
export function ms(value: number | null | undefined): string {
  if (value == null) return "–";
  if (value < 10) return `${value.toFixed(1)} ms`;
  if (value < 1000) return `${Math.round(value)} ms`;
  return `${(value / 1000).toFixed(value < 10_000 ? 2 : 1)} s`;
}

export function perMinute(value: number | null | undefined): string {
  return value == null ? "–" : `${value >= 10 ? Math.round(value) : value.toFixed(1)}/min`;
}
