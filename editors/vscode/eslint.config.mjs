import js from "@eslint/js";
import tseslint from "typescript-eslint";

export default tseslint.config(
  { ignores: ["dist", "node_modules", ".vscode-test"] },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  { files: ["*.mjs"], languageOptions: { globals: { process: "readonly", console: "readonly" } } },
  {
    rules: {
      "@typescript-eslint/no-unused-vars": ["error", { argsIgnorePattern: "^_" }],
    },
  },
);
