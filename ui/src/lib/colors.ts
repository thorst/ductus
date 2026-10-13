// The custom color scheme: five colors in, everything else derived, and a contrast check that can
// repair what it finds. Only the Appearance page loads this (its own chunk); the saved result is
// plain CSS values, which public/theme.js applies before the first paint.
//
// The built-in themes derive their muted text, borders and so on with CSS `color-mix()`. Here the
// same derivation is done in this file and written out as hex, so the numbers the check tests are
// exactly the numbers the browser draws.

export type Palette = { header: string; accent: string; bg: string; panel: string; text: string };
export type PaletteKey = keyof Palette;

export const PALETTE_KEYS: { key: PaletteKey; label: string; used: string }[] = [
  { key: "header", label: "Header", used: "The bar across the top" },
  { key: "accent", label: "Accent", used: "Links, buttons and the focus ring" },
  { key: "bg", label: "Page background", used: "Behind everything" },
  { key: "panel", label: "Panel", used: "Cards, tables and the menu" },
  { key: "text", label: "Text", used: "Everything you read" },
];

export const PRESETS: Record<string, Palette> = {
  Light: { header: "#17233b", accent: "#2f5bd3", bg: "#f3f5f8", panel: "#ffffff", text: "#17202b" },
  Dark: { header: "#0b111c", accent: "#7ca3ff", bg: "#0e131a", panel: "#151c26", text: "#e2e8f0" },
  Sepia: { header: "#4a3b2a", accent: "#8a4b08", bg: "#f4ecd8", panel: "#fbf6e9", text: "#241b12" },
  Ocean: { header: "#06293a", accent: "#5fd0e0", bg: "#0a1f2b", panel: "#0f2c3b", text: "#dcecf2" },
};

/** WCAG AA for text; the focus ring and buttons need 3:1 (1.4.11). */
export const AA = 4.5;
export const AA_GRAPHICS = 3;

// ---- color math -----------------------------------------------------------------------------

type Rgb = [number, number, number];

/** `#rgb` or `#rrggbb` (the `#` optional) as lower-case `#rrggbb`, or null. */
export function normalizeHex(s: string): string | null {
  const m = /^#?([0-9a-f]{3}|[0-9a-f]{6})$/i.exec(s.trim());
  if (!m) return null;
  const h = m[1]!.toLowerCase();
  return "#" + (h.length === 3 ? [...h].map((c) => c + c).join("") : h);
}

function parse(hex: string): Rgb {
  const n = parseInt(hex.slice(1), 16);
  return [(n >> 16) / 255, ((n >> 8) & 255) / 255, (n & 255) / 255];
}

function toHex(rgb: Rgb): string {
  const byte = (c: number) =>
    Math.round(Math.min(1, Math.max(0, c)) * 255)
      .toString(16)
      .padStart(2, "0");
  return "#" + rgb.map(byte).join("");
}

const linear = (c: number) => (c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4);
const gamma = (c: number) => (c <= 0.0031308 ? 12.92 * c : 1.055 * c ** (1 / 2.4) - 0.055);

export function luminance(hex: string): number {
  const [r, g, b] = parse(hex).map(linear) as Rgb;
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

/** WCAG contrast ratio, 1 to 21. */
export function contrast(a: string, b: string): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x) as [number, number];
  return (hi + 0.05) / (lo + 0.05);
}

type Lab = [number, number, number];

function toOklab(hex: string): Lab {
  const [r, g, b] = parse(hex).map(linear) as Rgb;
  const l = Math.cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b);
  const m = Math.cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b);
  const s = Math.cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b);
  return [
    0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s,
    1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s,
    0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s,
  ];
}

/** Linear sRGB for an Oklab color; outside 0..1 when the color is out of gamut. */
function fromOklab([L, a, b]: Lab): Rgb {
  const l = (L + 0.3963377774 * a + 0.2158037573 * b) ** 3;
  const m = (L - 0.1055613458 * a - 0.0638541728 * b) ** 3;
  const s = (L - 0.0894841775 * a - 1.291485548 * b) ** 3;
  return [
    4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
    -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
    -0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s,
  ];
}

const inGamut = (rgb: Rgb) => rgb.every((c) => c >= -1e-4 && c <= 1 + 1e-4);

/** Lightness, chroma and hue (OKLCH) to hex; chroma is reduced until the color fits sRGB. */
function fromOklch(L: number, C: number, h: number): string {
  let c = C;
  for (let i = 0; i < 60; i++, c *= 0.92) {
    const rgb = fromOklab([L, c * Math.cos(h), c * Math.sin(h)]);
    if (inGamut(rgb) || c < 1e-4) return toHex(rgb.map(gamma) as Rgb);
  }
  return toHex(fromOklab([L, 0, 0]).map(gamma) as Rgb);
}

function oklch(hex: string): { L: number; C: number; h: number } {
  const [L, a, b] = toOklab(hex);
  return { L, C: Math.hypot(a, b), h: Math.atan2(b, a) };
}

/** `weight` of `a` mixed with the rest of `b`. */
export function mix(a: string, b: string, weight: number): string {
  const x = toOklab(a);
  const y = toOklab(b);
  const lab = x.map((v, i) => v * weight + y[i]! * (1 - weight)) as Lab;
  return toHex(fromOklab(lab).map(gamma) as Rgb);
}

// ---- what the five colors give ---------------------------------------------------------------

export interface Derived {
  /** CSS custom properties (`--text`: `#...`) for `<html>`. */
  vars: Record<string, string>;
  scheme: "light" | "dark";
  muted: string;
  hover: string;
  accentSoft: string;
  headerFg: string;
  accentFg: string;
  status: string[];
}

const readableOn = (bg: string) => (contrast(bg, "#ffffff") >= contrast(bg, "#000000") ? "#ffffff" : "#000000");
const isDark = (...bgs: string[]) =>
  bgs.reduce((n, b) => n + contrast(b, "#ffffff"), 0) > bgs.reduce((n, b) => n + contrast(b, "#000000"), 0);

const STATUS_LIGHT = { ok: "#17733f", warn: "#8f5300", bad: "#b8232a", paused: "#5450c4" };
const STATUS_DARK = { ok: "#4cc987", warn: "#f0b04a", bad: "#ff7272", paused: "#a9a6ff" };

/** The lightness-moved color that first reaches `min` against every background, else the extreme. */
function readable(color: string, backgrounds: string[], min: number, lighter: boolean): string {
  const ok = (c: string) => backgrounds.every((b) => contrast(c, b) >= min);
  const found = search(color, ok, lighter);
  return found ?? (lighter ? "#ffffff" : "#000000");
}

/** The four status colors for these backgrounds (ok, warn, bad, paused). The slow part of `derive`. */
function statusColors(p: Palette): string[] {
  const dark = isDark(p.bg, p.panel);
  return Object.values(dark ? STATUS_DARK : STATUS_LIGHT).map((c) => readable(c, [p.bg, p.panel], 5, dark));
}

/** The shades mixed from the five colors (what the stylesheet's `color-mix()` does for the built-in themes). */
function shades(p: Palette) {
  return {
    muted: mix(p.text, p.panel, 0.66),
    hover: mix(p.text, p.panel, 0.05),
    accentSoft: mix(p.accent, p.panel, 0.13),
    headerFg: readableOn(p.header),
    accentFg: readableOn(p.accent),
  };
}

export function derive(p: Palette): Derived {
  const dark = isDark(p.bg, p.panel);
  const status = statusColors(p);
  const [ok, warn, bad, paused] = status as [string, string, string, string];
  const { muted, hover, accentSoft, headerFg, accentFg } = shades(p);
  return {
    scheme: dark ? "dark" : "light",
    muted,
    hover,
    accentSoft,
    headerFg,
    accentFg,
    status,
    vars: {
      "--header": p.header,
      "--header-fg": headerFg,
      "--accent": p.accent,
      "--accent-fg": accentFg,
      "--bg": p.bg,
      "--panel": p.panel,
      "--text": p.text,
      "--ok": ok,
      "--warn": warn,
      "--bad": bad,
      "--paused": paused,
      "--muted": muted,
      "--border": mix(p.text, p.panel, 0.14),
      "--hover": hover,
      "--accent-soft": accentSoft,
    },
  };
}

// ---- the check -------------------------------------------------------------------------------

/** Which of the user's colors a failing pair is repaired through. `derived` ones can't fail. */
export type Group = "text" | "accent" | "derived";

export interface Check {
  id: string;
  /** In words: what is read, on what. */
  label: string;
  fg: string;
  bg: string;
  min: number;
  group: Group;
  ratio: number;
  pass: boolean;
}

const worstStatus = (p: Palette) =>
  statusColors(p).reduce((w, c) => (contrast(c, p.panel) < contrast(w, p.panel) ? c : w));

/**
 * Every pair of colors the interface actually draws text or controls with. `withStatus: false`
 * leaves out the status colors' row (they're derived and always readable; working them out is
 * the slow part), which is all `fix` needs.
 */
export function check(p: Palette, withStatus = true): Check[] {
  const d = shades(p);
  const row = (id: string, label: string, fg: string, bg: string, min: number, group: Group): Check => {
    const ratio = contrast(fg, bg);
    return { id, label, fg, bg, min, group, ratio, pass: ratio >= min };
  };
  return [
    row("text-bg", "Text on the page background", p.text, p.bg, AA, "text"),
    row("text-panel", "Text on a panel", p.text, p.panel, AA, "text"),
    row("text-hover", "Text on a highlighted row", p.text, d.hover, AA, "text"),
    row("muted-bg", "Secondary text on the page background", d.muted, p.bg, AA, "text"),
    row("muted-panel", "Secondary text on a panel", d.muted, p.panel, AA, "text"),
    row("link-bg", "Links on the page background", p.accent, p.bg, AA, "accent"),
    row("link-panel", "Links on a panel", p.accent, p.panel, AA, "accent"),
    row("link-active", "The open page in the menu", p.accent, d.accentSoft, AA, "accent"),
    row("focus", "Focus ring against the page background", p.accent, p.bg, AA_GRAPHICS, "accent"),
    row("button", "Button text on the accent", d.accentFg, p.accent, AA, "derived"),
    row("header", "Header text on the header", d.headerFg, p.header, AA, "derived"),
    ...(withStatus ? [row("status", "Status colors (worst) on a panel", worstStatus(p), p.panel, AA, "derived")] : []),
  ];
}

export const failing = (p: Palette, withStatus = true) => check(p, withStatus).filter((c) => !c.pass);

// ---- fix it ----------------------------------------------------------------------------------

/**
 * Walks a color's OKLCH lightness (hue and chroma kept) toward white or black, in steps, to the
 * first value `ok` accepts; null when neither way gets there. The color itself if it already does.
 */
function search(color: string, ok: (c: string) => boolean, lighter: boolean): string | null {
  if (ok(color)) return color;
  const { L, C, h } = oklch(color);
  for (const up of [lighter, !lighter]) {
    for (let i = 1; ; i++) {
      const next = L + (up ? i : -i) * 0.005;
      if (next < 0 || next > 1) break;
      const candidate = fromOklch(next, C, h);
      if (ok(candidate)) return candidate;
    }
    const extreme = up ? "#ffffff" : "#000000";
    if (ok(extreme)) return extreme;
  }
  return null;
}

export interface Fixed {
  palette: Palette;
  changes: { key: PaletteKey; from: string; to: string }[];
}

const passes = (p: Palette, group: Group, margin: number) =>
  check(p, false)
    .filter((c) => c.group === group)
    .every((c) => c.ratio >= c.min + margin);

/**
 * Repairs one group (`text` or `accent`) by moving that color's lightness away from the
 * backgrounds, just far enough, with a little margin (browsers composite hover shades). The
 * header, page background and panel are what gives a scheme its look, so they stay, unless no
 * text or accent color can be read on them at all (black page, white panel): then the panel is
 * pulled toward the page background, and only if that is not enough, both are moved.
 */
function fixGroup(p: Palette, group: "text" | "accent"): Palette {
  if (passes(p, group, 0)) return p;
  const lighter = isDark(p.bg, p.panel);
  const attempt = (q: Palette): Palette | null => {
    for (const margin of [0.1, 0]) {
      const color = search(q[group], (c) => passes({ ...q, [group]: c }, group, margin), lighter);
      if (color) return { ...q, [group]: color };
    }
    return null;
  };
  const direct = attempt(p);
  if (direct) return direct;
  for (let t = 1; t <= 10; t++) {
    const found = attempt({ ...p, panel: mix(p.bg, p.panel, t / 10) });
    if (found) return found;
  }
  // Both backgrounds toward the side the readable color is not on.
  const { L: lBg, C: cBg, h: hBg } = oklch(p.bg);
  const { L: lPanel, C: cPanel, h: hPanel } = oklch(p.panel);
  for (let i = 1; i <= 50; i++) {
    const shift = (lighter ? -1 : 1) * i * 0.02;
    const clamp = (v: number) => Math.min(1, Math.max(0, v));
    const moved = {
      ...p,
      bg: fromOklch(clamp(lBg + shift), cBg, hBg),
      panel: fromOklch(clamp(lPanel + shift), cPanel, hPanel),
    };
    const found = attempt(moved);
    if (found) return found;
  }
  return p;
}

function changesBetween(before: Palette, after: Palette): Fixed["changes"] {
  return PALETTE_KEYS.filter(({ key }) => before[key] !== after[key]).map(({ key }) => ({
    key,
    from: before[key],
    to: after[key],
  }));
}

/** Repairs the text or the accent (what a failing row is about). */
export function fix(p: Palette, group: "text" | "accent"): Fixed {
  const palette = fixGroup(p, group);
  return { palette, changes: changesBetween(p, palette) };
}

/** Repairs everything the check finds. */
export function fixAll(p: Palette): Fixed {
  let q = p;
  // Moving a background to repair one group can unsettle the other: go round until stable.
  for (let i = 0; i < 4 && failing(q, false).length > 0; i++) q = fixGroup(fixGroup(q, "text"), "accent");
  return { palette: q, changes: changesBetween(p, q) };
}
