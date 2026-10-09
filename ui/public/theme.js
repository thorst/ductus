// Runs before the page paints: applies the saved theme (src/lib/theme.ts keeps it), so a dark
// theme never flashes light. A file, not inline: the engine's CSP allows only its own scripts.
try {
  const theme = localStorage.getItem("ductus.theme");
  if (theme && theme !== "system") document.documentElement.dataset.theme = theme;
} catch {
  // Storage blocked: the system theme it is.
}
