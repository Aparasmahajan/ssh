/**
 * Desktop-build preflight.
 *
 * `npm run desktop:dev` fails with a fairly opaque error when the Rust toolchain
 * is missing, and on Windows it fails a second, different way when the MSVC
 * linker is missing. On Linux it fails a third way: `webkit2gtk-sys` panics in
 * its build script when pkg-config cannot find the WebKitGTK development
 * headers. This says which, up front, with the command that fixes it.
 *
 * Run:  npm run doctor
 */
import { execFileSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { platform } from "node:process";

const win = platform === "win32";
const linux = platform === "linux";
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

if (linux) {
  // Tauri v2 on Linux links WebKitGTK 4.1 (the libsoup3 build) — not 4.0, which
  // is what most older tutorials install. Probed through pkg-config because
  // that is exactly how the -sys crates find them; if pkg-config cannot, the
  // build cannot either.
  const pm = linuxPackages();

  check({
    name: "C compiler and linker (cc)",
    ok: have("cc"),
    fix: pm.install(pm.toolchain),
  });

  const pkgConfig = have("pkg-config");
  check({
    name: "pkg-config",
    ok: pkgConfig,
    fix: pm.install(pm.pkgconfig),
  });

  const libs = [
    ["webkit2gtk-4.1", "WebKitGTK 4.1 (the webview)"],
    ["javascriptcoregtk-4.1", "JavaScriptCoreGTK 4.1"],
    ["libsoup-3.0", "libsoup 3"],
    ["gtk+-3.0", "GTK 3"],
  ];
  const missing = pkgConfig ? libs.filter(([pc]) => !pkgExists(pc)) : libs;
  check({
    name: "WebKitGTK / GTK development libraries",
    ok: missing.length === 0,
    fix:
      (pkgConfig ? `missing: ${missing.map(([, n]) => n).join(", ")}\n      ` : "") +
      pm.install(pm.webkit),
  });

  // Not needed to compile; needed by the bundler for the AppImage/.deb icons.
  check({
    name: "librsvg (bundling only)",
    ok: pkgConfig && pkgExists("librsvg-2.0"),
    fix: pm.install(pm.rsvg),
    blocks: false,
  });

  // Agent auth on Linux is SSH_AUTH_SOCK, set by the desktop session (GNOME
  // Keyring, KDE's ksshaskpass setup, or `eval $(ssh-agent)`). A launcher icon
  // started outside such a session will not see it, and agent auth then fails
  // with "no SSH agent" even though `ssh` works in a terminal.
  check({
    name: "SSH agent reachable (SSH_AUTH_SOCK)",
    ok: !!process.env.SSH_AUTH_SOCK && existsSync(process.env.SSH_AUTH_SOCK),
    fix: "start an agent (`eval $(ssh-agent)`) or enable your desktop's keyring agent — key-file and vault auth work without one",
    blocks: false,
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

function pkgExists(name) {
  try {
    execFileSync("pkg-config", ["--exists", name], { stdio: "pipe" });
    return true;
  } catch {
    return false;
  }
}

/** Package names differ per distro family; detect from /etc/os-release. */
function linuxPackages() {
  let ids = "";
  try {
    const rel = readFileSync("/etc/os-release", "utf8");
    const field = (k) => rel.match(new RegExp(`^${k}=\"?([^\"\\n]*)`, "m"))?.[1] ?? "";
    ids = `${field("ID")} ${field("ID_LIKE")}`;
  } catch {
    /* no os-release: fall through to the generic hint */
  }
  const is = (...names) => names.some((n) => ids.split(/\s+/).includes(n));

  if (is("debian", "ubuntu")) {
    return {
      install: (p) => `sudo apt install ${p}`,
      toolchain: "build-essential",
      pkgconfig: "pkg-config",
      webkit: "libwebkit2gtk-4.1-dev",
      rsvg: "librsvg2-dev",
    };
  }
  if (is("fedora", "rhel", "centos")) {
    return {
      install: (p) => `sudo dnf install ${p}`,
      toolchain: "gcc",
      pkgconfig: "pkgconf-pkg-config",
      webkit: "webkit2gtk4.1-devel",
      rsvg: "librsvg2-devel",
    };
  }
  if (is("arch", "manjaro", "endeavouros")) {
    return {
      install: (p) => `sudo pacman -S --needed ${p}`,
      toolchain: "base-devel",
      pkgconfig: "pkgconf",
      webkit: "webkit2gtk-4.1",
      rsvg: "librsvg",
    };
  }
  if (is("opensuse", "suse", "opensuse-tumbleweed", "opensuse-leap")) {
    return {
      install: (p) => `sudo zypper in ${p}`,
      toolchain: "-t pattern devel_basis",
      pkgconfig: "pkg-config",
      webkit: "webkit2gtk3-devel libsoup3-devel",
      rsvg: "librsvg-devel",
    };
  }
  return {
    install: (p) => `install the ${p} development package(s) for your distribution`,
    toolchain: "C compiler",
    pkgconfig: "pkg-config",
    webkit: "WebKitGTK 4.1",
    rsvg: "librsvg",
  };
}

console.log(
  blocking === 0
    ? "\n  Ready. `npm run desktop:dev` should launch the desktop window.\n"
    : `\n  ${blocking} blocking item(s). The desktop app cannot build until they are resolved;` +
        " `npm run dev` still serves the UI in a browser.\n",
);

process.exit(blocking === 0 ? 0 : 1);
