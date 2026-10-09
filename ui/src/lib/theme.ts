// The theme: kept in this browser until users exist (step 8), then per user on the server.
// public/theme.js applies it before the first paint; this changes it while the app runs.

export const THEMES = ["system", "light", "dark", "contrast"] as const;
export type Theme = (typeof THEMES)[number];

export const THEME_LABELS: Record<Theme, string> = {
  system: "System",
  light: "Light",
  dark: "Dark",
  contrast: "High contrast",
};

const KEY = "ductus.theme";

export function getTheme(): Theme {
  try {
    const saved = localStorage.getItem(KEY);
    return THEMES.find((t) => t === saved) ?? "system";
  } catch {
    return "system";
  }
}

export function setTheme(theme: Theme) {
  const root = document.documentElement;
  if (theme === "system") delete root.dataset.theme;
  else root.dataset.theme = theme;
  try {
    localStorage.setItem(KEY, theme);
  } catch {
    // Storage blocked: it still applies until the page is reloaded.
  }
}
