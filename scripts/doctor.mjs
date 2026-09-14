/**
 * Desktop-build preflight.
 *
 * `npm run desktop:dev` fails with a fairly opaque error when the Rust toolchain
 * is missing, and on Windows it fails a second, different way when the MSVC
 * linker is missing. This says which, up front, with the command that fixes it.
 *
 * Run:  npm run doctor
 */
import { execFileSync } from "node:child_process";
import { existsSync } from "node:fs";
import { platform } from "node:process";

const win = platform === "win32";
let blocking = 0;

function have(cmd) {
  try {
    execFileSync(win ? "where" : "which", [cmd], { stdio: "pipe" });
    return true;
  } catch {
    return false;
  }
}

function check({ name, ok, fix, blocks = true }) {
  if (ok) {
    console.log(`  ✓ ${name}`);
    return;
  }
  console.log(`  ${blocks ? "✗" : "!"} ${name}`);
  console.log(`      ${fix}`);
  if (blocks) blocking++;
}

console.log("\n  OpsPilot — desktop build prerequisites\n");

check({
  name: "Rust toolchain (cargo)",
  ok: have("cargo"),
  fix: win
    ? "winget install Rustlang.Rustup   — then reopen the shell"
    : "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh",
});

if (win) {
  const vswhere = "C:\\Program Files (x86)\\Microsoft Visual Studio\\Installer\\vswhere.exe";
  let msvc = false;
  if (existsSync(vswhere)) {
    try {
      const out = execFileSync(
        vswhere,
        ["-latest", "-products", "*", "-requires", "Microsoft.VisualStudio.Component.VC.Tools.x86.x64", "-property", "installationPath"],
        { encoding: "utf8" },
      );
      msvc = out.trim().length > 0;
    } catch {
      msvc = false;
    }
  }
  check({
    name: "MSVC build tools (linker for the windows-msvc target)",
    ok: msvc,
    fix:
      'winget install Microsoft.VisualStudio.2022.BuildTools --override "--wait --passive ' +
      '--add Microsoft.VisualStudio.Component.VC.Tools.x86.x64 ' +
      '--add Microsoft.VisualStudio.Component.Windows11SDK.22621"   — needs admin, several GB',
  });
}

check({
  name: "@tauri-apps/cli",
  ok: existsSync("node_modules/@tauri-apps/cli"),
  fix: "npm install",
});

// Blocking on Windows, and this was originally mis-marked as optional. tauri-build
// generates a Windows Resource file during the build script, so a missing icon.ico
// fails `cargo check --workspace` — not merely `tauri build`.
check({
  name: win ? "icons (icon.ico — required to compile, not just to bundle)" : "bundle icons",
  ok: existsSync("src-tauri/icons/icon.ico"),
  fix: "node scripts/make-icon.mjs && npx tauri icon src-tauri/icons/source.png",
  blocks: win,
});

console.log(
  blocking === 0
    ? "\n  Ready. `npm run desktop:dev` should launch the desktop window.\n"
    : `\n  ${blocking} blocking item(s). The desktop app cannot build until they are resolved;` +
        " `npm run dev` still serves the UI in a browser.\n",
);

process.exit(blocking === 0 ? 0 : 1);
