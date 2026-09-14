import { test, expect } from "@playwright/test";

/**
 * OpsPilot — visual fidelity harness.
 *
 * This is the mechanism that keeps the built UI matching the Figma file.
 * Nothing else enforces it: Claude Code cannot open Figma, measure it, or
 * diff against it.
 *
 * SETUP (once):
 *   1. In Figma, select the "App Shell — Session View" frame (node 3:2) and
 *      export at 1x PNG. Save to  tests/visual/baseline/session-view.png
 *      Repeat for "SFTP — Dual Pane" (node 8:2) → sftp-dual-pane.png
 *      https://www.figma.com/design/wkE7H0umKnwHYtijV6oXXo
 *   2. Frames are 1600x1000 and 1180x800. The viewports below match exactly;
 *      a mismatched viewport makes every diff meaningless.
 *
 * Run:  npx playwright test visual-check
 * Accept intentional changes:  npx playwright test --update-snapshots
 *
 * IMPORTANT: a failing diff is not automatically a bug in the code. Read
 * UI_BUILD_GUIDE §0.9 — the guide is knowingly stale on host row height
 * (Figma's 64px three-line row is correct, not the guide's 44px), and Figma
 * can never show hover, focus, motion, or virtualized scroll state.
 */

const MASK_SELECTORS = [
  "[data-live]",        // RTT sparklines, throughput readouts, clocks
  ".link-strip",        // 10Hz canvas — never pixel-stable
  "[data-timestamp]",   // log lines, file mtimes
];

test.describe("visual fidelity vs Figma", () => {
  test.beforeEach(async ({ page }) => {
    // Freeze anything that ticks, or every run diffs against itself.
    await page.addInitScript(() => {
      // deterministic clock
      const FIXED = new Date("2026-07-30T08:41:03Z").getTime();
      Date.now = () => FIXED;
      // deterministic RTT samples for the link strip
      (window as any).__OPSPILOT_TEST_FIXTURES__ = true;
      Math.random = () => 0.42;
    });
  });

  test("session view matches baseline", async ({ page }) => {
    await page.setViewportSize({ width: 1600, height: 1000 });
    await page.goto("/?fixture=session-view");

    // Wait for fonts. Without this the first run screenshots the fallback
    // face and every subsequent run "fails" — the #1 false positive here.
    await page.evaluate(() => document.fonts.ready);
    await expect(page.locator("[data-testid=app-shell]")).toBeVisible();

    await expect(page).toHaveScreenshot("session-view.png", {
      maxDiffPixelRatio: 0.02,
      mask: MASK_SELECTORS.map((s) => page.locator(s)),
      animations: "disabled",
    });
  });

  test("sftp dual pane matches baseline", async ({ page }) => {
    await page.setViewportSize({ width: 1180, height: 800 });
    await page.goto("/?fixture=sftp-dual-pane");
    await page.evaluate(() => document.fonts.ready);

    await expect(page).toHaveScreenshot("sftp-dual-pane.png", {
      maxDiffPixelRatio: 0.02,
      mask: MASK_SELECTORS.map((s) => page.locator(s)),
      animations: "disabled",
    });
  });
});

/**
 * Token-level assertions. These catch drift that a screenshot diff can miss
 * or that a generous maxDiffPixelRatio would tolerate.
 */
test.describe("design system invariants", () => {
  test("fonts actually loaded — no fallback", async ({ page }) => {
    await page.goto("/");
    await page.evaluate(() => document.fonts.ready);

    const loaded = await page.evaluate(() => ({
      sans: document.fonts.check('500 12px "IBM Plex Sans"'),
      condensed: document.fonts.check('600 9px "IBM Plex Sans Condensed"'),
      mono: document.fonts.check('400 11px "IBM Plex Mono"'),
      term: document.fonts.check('400 12.5px "JetBrains Mono"'),
    }));

    // If any of these is false the app renders in system-ui and looks like a
    // generic dashboard. This is the single highest-impact fidelity failure.
    expect(loaded).toEqual({ sans: true, condensed: true, mono: true, term: true });
  });

  test("box-drawing glyphs survived font subsetting", async ({ page }) => {
    await page.goto("/");
    await page.evaluate(() => document.fonts.ready);

    // systemd trees and `tree` output render as tofu if U+2500-257F was
    // stripped. Compare advance widths: a tofu box differs from a real glyph.
    const ok = await page.evaluate(() => {
      const c = document.createElement("canvas").getContext("2d")!;
      c.font = '400 12.5px "JetBrains Mono"';
      const box = c.measureText("├").width;
      const ref = c.measureText("M").width;
      return box > 0 && Math.abs(box - ref) < 0.6; // monospace: equal advance
    });
    expect(ok).toBe(true);
  });

  test("cyan is never used as an action colour", async ({ page }) => {
    await page.goto("/?fixture=session-view");
    await page.evaluate(() => document.fonts.ready);

    // Cyan means link state ONLY. If a button or link adopts it, "connected"
    // stops being readable at a glance — see UI_BUILD_GUIDE §0.2.
    const violations = await page.evaluate(() => {
      const cyan = getComputedStyle(document.documentElement)
        .getPropertyValue("--cyan-400").trim().toLowerCase();
      const toHex = (rgb: string) => {
        const m = rgb.match(/\d+/g);
        return m ? "#" + m.slice(0, 3).map(n => (+n).toString(16).padStart(2, "0")).join("") : "";
      };
      return [...document.querySelectorAll("button, a, [role=button]")]
        .filter(el => {
          const s = getComputedStyle(el);
          return toHex(s.color) === cyan || toHex(s.backgroundColor) === cyan;
        })
        .map(el => el.outerHTML.slice(0, 120));
    });
    expect(violations).toEqual([]);
  });

  test("all numerics use tabular figures", async ({ page }) => {
    await page.goto("/?fixture=session-view");
    await page.evaluate(() => document.fonts.ready);

    // A rate readout whose digits change width reads as broken even when the
    // number is correct.
    const bad = await page.evaluate(() =>
      [...document.querySelectorAll(".num")]
        .filter(el => !getComputedStyle(el).fontVariantNumeric.includes("tabular-nums"))
        .map(el => el.textContent?.slice(0, 40) ?? "")
    );
    expect(bad).toEqual([]);
  });

  test("focus is always visible", async ({ page }) => {
    await page.goto("/?fixture=session-view");
    const bad = await page.evaluate(() =>
      [...document.querySelectorAll("button, a, input, [tabindex]")]
        .filter(el => {
          const s = getComputedStyle(el, ":focus-visible");
          return s.outlineStyle === "none" && s.boxShadow === "none";
        }).length
    );
    expect(bad).toBe(0);
  });

  test("hairlines hold at fractional DPI", async ({ browser }) => {
    // At 125%/150% Windows scaling a 1px border lands on a half pixel and
    // blurs or vanishes. The 3px env tie bar is the most visible casualty.
    for (const scale of [1, 1.25, 1.5]) {
      const ctx = await browser.newContext({ deviceScaleFactor: scale });
      const page = await ctx.newPage();
      await page.setViewportSize({ width: 1600, height: 1000 });
      await page.goto("/?fixture=session-view");
      await page.evaluate(() => document.fonts.ready);

      const tie = page.locator("[data-testid=env-tie]").first();
      await expect(tie).toBeVisible();
      const box = await tie.boundingBox();
      expect(box!.width).toBeGreaterThanOrEqual(2.5); // never collapses
      await ctx.close();
    }
  });
});
