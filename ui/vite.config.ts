import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { fileURLToPath } from "node:url";

// `npm run dev` serves the UI with hot reload and sends /api to a running engine
// (DUCTUS_API_URL, default the engine's own default address).
const engine = process.env.DUCTUS_API_URL ?? "http://127.0.0.1:7700";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: { alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) } },
  server: { proxy: { "/api": { target: engine, changeOrigin: false } } },
  build: {
    // The engine embeds dist/ (crates/ductus-runtime/build.rs); no source maps in the binary.
    outDir: "dist",
    sourcemap: false,
    // scripts/budget.mjs holds the real limit (gzipped, what's downloaded); this one counts raw bytes.
    chunkSizeWarningLimit: 1000,
  },
  test: {
    environment: "jsdom",
    include: ["src/**/*.test.{ts,tsx}"],
    setupFiles: ["src/test-setup.ts"],
  },
});
