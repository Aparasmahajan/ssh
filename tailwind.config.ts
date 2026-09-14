import type { Config } from "tailwindcss";

/**
 * OpsPilot Tailwind config.
 *
 * Every value maps to a CSS variable in tokens.css. Nothing is defined twice.
 *
 * The `borderRadius`, `fontFamily`, and `fontSize` keys below are REPLACED,
 * not extended. Tailwind's defaults are the single most common cause of a
 * build drifting from the Figma file: default radii round corners the design
 * wants at 3px, and the default font stack silently swaps IBM Plex for
 * system-ui, which is most of the visual identity.
 */
export default {
  content: ["./index.html", "./src/**/*.{ts,tsx}"],
  darkMode: "class",
  theme: {
    // ---- REPLACED (not extended) --------------------------------------
    borderRadius: {
      none: "0",
      sm: "var(--radius-sm)",
      DEFAULT: "var(--radius)",
      full: "9999px",
    },
    fontFamily: {
      sans: "var(--font-sans)",
      condensed: "var(--font-condensed)",
      mono: "var(--font-mono)",
      term: "var(--font-term)",
    },
    // Line height is bound to size so it can never be forgotten.
    fontSize: {
      eyebrow: ["var(--text-eyebrow)", { lineHeight: "var(--leading-eyebrow)", letterSpacing: "var(--tracking-eyebrow)" }],
      micro:   ["var(--text-micro)",   { lineHeight: "var(--leading-micro)" }],
      small:   ["var(--text-small)",   { lineHeight: "var(--leading-small)" }],
      base:    ["var(--text-base)",    { lineHeight: "var(--leading-base)" }],
      large:   ["var(--text-large)",   { lineHeight: "var(--leading-large)" }],
      display: ["var(--text-display)", { lineHeight: "var(--leading-display)" }],
    },

    extend: {
      colors: {
        chassis: {
          900: "var(--chassis-900)",
          800: "var(--chassis-800)",
          700: "var(--chassis-700)",
        },
        rail:   { 600: "var(--rail-600)" },
        bezel:  { 500: "var(--bezel-500)" },
        text: {
          hi:  "var(--text-hi)",
          mid: "var(--text-mid)",
          lo:  "var(--text-lo)",
        },
        amber:  { 500: "var(--amber-500)", 900: "var(--amber-900)" },
        cyan:   { 400: "var(--cyan-400)" },
        red:    { 500: "var(--red-500)" },
        violet: { 400: "var(--violet-400)" },
        env: {
          prod:     "var(--env-prod)",
          stage:    "var(--env-stage)",
          dev:      "var(--env-dev)",
          test:     "var(--env-test)",
          customer: "var(--env-customer)",
        },
      },
      borderWidth: {
        hairline: "var(--hairline)",
        tie: "3px",       // environment cable-tie bar
      },
      spacing: {
        rail: "var(--rail-w)",
        tree: "var(--tree-w)",
        inspector: "var(--inspector-w)",
        status: "var(--status-h)",
        tabstrip: "var(--tabstrip-h)",
        compact: "var(--row-h-compact)",
      },
      transitionTimingFunction: { DEFAULT: "var(--ease)" },
      transitionDuration: {
        hover: "var(--dur-hover)",
        panel: "var(--dur-panel)",
      },
      keyframes: {
        // "connecting" state on a host status dot
        pulse: {
          "0%,100%": { opacity: "1" },
          "50%":     { opacity: "0.35" },
        },
      },
      animation: { pulse: "pulse 1.2s var(--ease) infinite" },
    },
  },
  corePlugins: {
    // The shell owns all scrolling and layout; these only cause surprises.
    container: false,
    preflight: true, // keep the reset, but see base overrides in tokens.css
  },
  plugins: [],
} satisfies Config;
