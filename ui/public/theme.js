// Runs before the page paints: applies the saved theme (src/lib/theme.ts keeps it), so a dark
// theme never flashes light. A file, not inline: the engine's CSP allows only its own scripts.
// A custom theme is its saved CSS values (src/lib/colors.ts worked them out), set on <html>.
try {
  const theme = localStorage.getItem("ductus.theme");
  if (theme && theme !== "system") {
    const root = document.documentElement;
    if (theme === "custom") {
      const saved = JSON.parse(localStorage.getItem("ductus.colors") ?? "null");
      if (saved && saved.vars) {
        for (const [name, value] of Object.entries(saved.vars)) root.style.setProperty(name, value);
        root.style.setProperty("color-scheme", saved.scheme);
        root.dataset.theme = theme;
      }
    } else {
      root.dataset.theme = theme;
    }
  }
} catch {
  // Storage blocked, or nothing usable saved: the system theme it is.
}
