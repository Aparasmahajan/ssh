import React from "react";
import ReactDOM from "react-dom/client";

/* Import order matters and is load-bearing:
   1. fonts.css   — @font-face first, so `font-display: block` is armed before
                    the first paint (UI_BUILD_GUIDE §0.9, failure mode #1).
   2. tailwind.css — Preflight + utilities.
   3. tokens.css  — variables AND the base html/body rules, which must win
                    over Preflight. */
import "./styles/fonts.css";
import "./styles/tailwind.css";
import "./styles/tokens.css";

import { App } from "./App";

const root = document.getElementById("root");
if (!root) throw new Error("#root missing from index.html");

ReactDOM.createRoot(root).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
