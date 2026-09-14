import js from "@eslint/js";
import tsParser from "@typescript-eslint/parser";
import tsPlugin from "@typescript-eslint/eslint-plugin";
import reactHooks from "eslint-plugin-react-hooks";

/**
 * The rule that matters here is `no-restricted-syntax` on hex literals.
 *
 * UI_BUILD_GUIDE §0.9: "Tokens are the design; Figma is only a rendering of
 * them." Without a build-time failure the palette erodes within two weeks —
 * someone inlines #1B1F26, it drifts one shade, and nobody notices for months.
 * A wrong colour must be a lint error, not an observation.
 */
const NO_HEX_MESSAGE =
  "No hex colour literals. Every colour comes from a token in src/styles/tokens.css " +
  "(use a Tailwind class or var(--token)).";

const HEX = "/#(?:[0-9a-fA-F]{3,4}|[0-9a-fA-F]{6}|[0-9a-fA-F]{8})(?![0-9a-fA-F])/";

export default [
  { ignores: ["dist/**", "node_modules/**", "src-tauri/**", "target/**"] },
  js.configs.recommended,
  {
    files: ["**/*.{ts,tsx}"],
    languageOptions: {
      parser: tsParser,
      parserOptions: { ecmaVersion: 2022, sourceType: "module", ecmaFeatures: { jsx: true } },
      globals: { window: "readonly", document: "readonly", navigator: "readonly", console: "readonly" },
    },
    plugins: { "@typescript-eslint": tsPlugin, "react-hooks": reactHooks },
    rules: {
      ...tsPlugin.configs.recommended.rules,
      ...reactHooks.configs.recommended.rules,
      "no-undef": "off", // TypeScript already does this, and better
      "@typescript-eslint/no-unused-vars": ["error", { argsIgnorePattern: "^_" }],
      "no-restricted-syntax": [
        "error",
        { selector: `Literal[value=${HEX}]`, message: NO_HEX_MESSAGE },
        { selector: `TemplateElement[value.raw=${HEX}]`, message: NO_HEX_MESSAGE },
      ],
    },
  },
  {
    // Test harnesses reach into `window` and monkey-patch `Date.now`; the
    // provided visual-check.spec.ts is authoritative and is not edited to
    // satisfy a lint rule. The no-hex rule above still applies here.
    files: ["tests/**/*.ts", "*.config.ts"],
    rules: { "@typescript-eslint/no-explicit-any": "off" },
  },
];
