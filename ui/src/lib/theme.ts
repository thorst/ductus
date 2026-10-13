// The theme. A signed-in user's choice lives on the server (src/api/appearance.ts keeps it
// there); this browser keeps a copy too, because public/theme.js applies it before the first
// paint (no flash of the wrong theme) and because a session made with a token has no account to
// save it in. This file reads and writes that copy and tells the listener (the sync) about changes.
//
// `custom` is the user's own five colors (src/lib/colors.ts works out the rest). What is saved
// is the finished CSS values, so this file and theme.js need none of the color math: they only
// set variables. Those go inline on <html>, which beats the stylesheet's `:root` values.

export const THEMES = ["system", "light", "dark", "contrast", "custom"] as const;
export type Theme = (typeof THEMES)[number];

export const THEME_LABELS: Record<Theme, string> = {
  system: "System",
  light: "Light",
  dark: "Dark",
  contrast: "High contrast",
  custom: "Custom",
};

const KEY = "ductus.theme";
const COLORS_KEY = "ductus.colors";

/** The five chosen colors and the CSS values worked out from them. */
export type SavedColors = {
  colors: Record<"header" | "accent" | "bg" | "panel" | "text", string>;
  vars: Record<string, string>;
  scheme: "light" | "dark";
};

/** Everything a person chooses about how the UI looks. */
export type Appearance = { theme: Theme; colors: SavedColors | null };

/** Every variable a custom theme sets, so leaving it can take them off again. */
const CUSTOM_VARS = [
  "--header",
  "--header-fg",
  "--accent",
  "--accent-fg",
  "--bg",
  "--panel",
  "--text",
  "--ok",
  "--warn",
  "--bad",
  "--paused",
  "--muted",
  "--border",
  "--hover",
  "--accent-soft",
];

/** Told after a person changes the appearance (not when one is applied from the server). */
let listener: ((appearance: Appearance) => void) | null = null;

export function onAppearanceChange(fn: ((appearance: Appearance) => void) | null) {
  listener = fn;
}

function notify() {
  listener?.(currentAppearance());
}

export function isSavedColors(saved: unknown): saved is SavedColors {
  return !!saved && typeof saved === "object" && "vars" in saved && "colors" in saved && "scheme" in saved;
}

export function getSavedColors(): SavedColors | null {
  try {
    const saved: unknown = JSON.parse(localStorage.getItem(COLORS_KEY) ?? "null");
    return isSavedColors(saved) ? saved : null;
  } catch {
    return null;
  }
}

export function getTheme(): Theme {
  try {
    const saved = localStorage.getItem(KEY);
    const theme = THEMES.find((t) => t === saved) ?? "system";
    // Custom without colors to show (storage cleared in part) is the system theme.
    return theme === "custom" && !getSavedColors() ? "system" : theme;
  } catch {
    return "system";
  }
}

export function currentAppearance(): Appearance {
  return { theme: getTheme(), colors: getSavedColors() };
}

function applyColors(saved: SavedColors | null) {
  const style = document.documentElement.style;
  for (const name of CUSTOM_VARS) style.removeProperty(name);
  style.removeProperty("color-scheme");
  if (!saved) return;
  for (const [name, value] of Object.entries(saved.vars)) style.setProperty(name, value);
  style.setProperty("color-scheme", saved.scheme);
}

function remember(key: string, value: string | null) {
  try {
    if (value === null) localStorage.removeItem(key);
    else localStorage.setItem(key, value);
  } catch {
    // Storage blocked: it applies until the page is reloaded.
  }
}

/** Shows `theme` (and `colors` for a custom one) and keeps them in this browser. Tells nobody. */
function show(theme: Theme, colors: SavedColors | null) {
  const root = document.documentElement;
  if (theme === "system") delete root.dataset.theme;
  else root.dataset.theme = theme;
  applyColors(theme === "custom" ? colors : null);
  remember(KEY, theme);
}

export function setTheme(theme: Theme) {
  show(theme, getSavedColors());
  notify();
}

/** Keeps a custom scheme and switches to it. */
export function saveCustomColors(saved: SavedColors) {
  remember(COLORS_KEY, JSON.stringify(saved));
  // Applied directly: with storage blocked, reading back what was just saved would find nothing.
  show("custom", saved);
  notify();
}

/** Forgets the custom scheme; a custom theme falls back to following the system. */
export function forgetCustomColors() {
  remember(COLORS_KEY, null);
  if (document.documentElement.dataset.theme === "custom") show("system", null);
  notify();
}

/** Applies what the server has saved for this person (and keeps it here for the first paint). */
export function applyAppearance(appearance: Appearance) {
  remember(COLORS_KEY, appearance.colors ? JSON.stringify(appearance.colors) : null);
  const theme = appearance.theme === "custom" && !appearance.colors ? "system" : appearance.theme;
  show(theme, appearance.colors);
}

/** After a person signs out their choices stay on the server: this browser goes back to the default. */
export function forgetAppearanceHere() {
  remember(COLORS_KEY, null);
  show("system", null);
}

/** Has this browser anything of its own to offer a new account? */
export function hasLocalChoice(): boolean {
  const { theme, colors } = currentAppearance();
  return theme !== "system" || colors !== null;
}
