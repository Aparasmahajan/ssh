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

import { invoke } from "@tauri-apps/api/core";
import { App } from "./App";
import { IS_DESKTOP } from "./lib/runtime";

const root = document.getElementById("root");
if (!root) throw new Error("#root missing from index.html");

/* Release whatever a previous page load left in the Rust core — terminal
   sessions, database connections, Docker and Kubernetes watches. This page's
   state starts empty, so after a webview reload all of those are unreachable.
   Awaited BEFORE rendering: run concurrently, it could close sessions this
   page had already opened. A failure is not fatal; the app still starts. */
const reset: Promise<unknown> = IS_DESKTOP
  ? invoke<number>("app_reset").catch(() => 0)
  : Promise.resolve(0);

void reset.then(() =>
  ReactDOM.createRoot(root).render(
    <React.StrictMode>
      <App />
    </React.StrictMode>,
  ),
);
