// The theme: kept in this browser until users exist (step 8), then per user on the server.
// public/theme.js applies it before the first paint; this changes it while the app runs.
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

export function getSavedColors(): SavedColors | null {
  try {
    const saved: unknown = JSON.parse(localStorage.getItem(COLORS_KEY) ?? "null");
    const ok = saved && typeof saved === "object" && "vars" in saved && "colors" in saved && "scheme" in saved;
    return ok ? (saved as SavedColors) : null;
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

function applyColors(saved: SavedColors | null) {
  const style = document.documentElement.style;
  for (const name of CUSTOM_VARS) style.removeProperty(name);
  style.removeProperty("color-scheme");
  if (!saved) return;
  for (const [name, value] of Object.entries(saved.vars)) style.setProperty(name, value);
  style.setProperty("color-scheme", saved.scheme);
}

export function setTheme(theme: Theme) {
  const root = document.documentElement;
  if (theme === "system") delete root.dataset.theme;
  else root.dataset.theme = theme;
  applyColors(theme === "custom" ? getSavedColors() : null);
  try {
    localStorage.setItem(KEY, theme);
  } catch {
    // Storage blocked: it still applies until the page is reloaded.
  }
}

/** Keeps a custom scheme and switches to it. */
export function saveCustomColors(saved: SavedColors) {
  try {
    localStorage.setItem(COLORS_KEY, JSON.stringify(saved));
  } catch {
    // Storage blocked: it applies until the page is reloaded.
  }
  // Applying reads what was just saved; with storage blocked, apply it directly.
  setTheme("custom");
  applyColors(saved);
}

/** Forgets the custom scheme; a custom theme falls back to following the system. */
export function forgetCustomColors() {
  try {
    localStorage.removeItem(COLORS_KEY);
  } catch {
    // Nothing kept to forget.
  }
  if (document.documentElement.dataset.theme === "custom") setTheme("system");
}
