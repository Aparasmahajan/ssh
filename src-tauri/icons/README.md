# Icons

Generated, not hand-drawn. `source.png` comes from `scripts/make-icon.mjs`, which
reads the palette out of `src/styles/tokens.css` so the icon cannot drift from the
design system. Everything else in this folder is fanned out from it by
`npx tauri icon`.

To regenerate after a palette or mark change:

```bash
node scripts/make-icon.mjs && npx tauri icon src-tauri/icons/source.png
```

## `icon.ico` is required to BUILD, not just to bundle

An earlier version of this file said `tauri dev` runs without icons and only
`tauri build` needs them. **That is wrong on Windows.** `tauri-build` generates a
Windows Resource file during the build script, and without `icons/icon.ico` even
`cargo check --workspace` fails:

```
`icons/icon.ico` not found; required for generating a Windows Resource file during tauri-build
```

`scripts/doctor.mjs` treats icons as a blocking prerequisite on Windows for this
reason.

## The mark

The product thesis at icon scale (UI_BUILD_GUIDE §0.1): a faceplate with the amber
environment tie bar down the left and port chips beside it. Flat fills only — the
same constraint as the UI, and the thing that survives being scaled to 16px. Cyan
appears once, as a link LED, which is the only thing §0.2 allows cyan to mean.
