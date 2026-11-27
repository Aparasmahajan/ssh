//! Which shells can this machine actually start?
//!
//! A terminal that only runs `cmd.exe` on Windows is half a tool. Git for
//! Windows, WSL and MSYS2 are all commonly installed and all speak a POSIX
//! shell, which is what most people actually want a terminal for.
//!
//! Discovery is by probing the filesystem and `PATH` — nothing is offered that
//! is not present, so the UI never lists a shell that fails to launch.

use serde::Serialize;

#[derive(Serialize, Clone, Debug)]
pub struct ShellInfo {
    /// Stable key the frontend passes back to `session_open`.
    pub id: String,
    pub label: String,
    pub program: String,
    pub args: Vec<String>,
    /// Extra environment. Git Bash in particular misbehaves without it.
    pub env: Vec<(String, String)>,
}

fn exists(p: &str) -> bool {
    std::path::Path::new(p).is_file()
}

/// Look up a bare command name on `PATH`. Windows-only: Unix discovery reads
/// `/etc/shells` instead, so this would be dead code there.
#[cfg(windows)]
fn on_path(name: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|c| c.is_file())
        .map(|c| c.to_string_lossy().into_owned())
}

#[cfg(windows)]
fn candidates() -> Vec<ShellInfo> {
    let mut out = Vec::new();

    if let Ok(comspec) = std::env::var("COMSPEC") {
        if exists(&comspec) {
            out.push(ShellInfo {
                id: "cmd".into(),
                label: "Command Prompt".into(),
                program: comspec,
                args: vec![],
                env: vec![],
            });
        }
    }

    if let Some(ps) = on_path("powershell.exe") {
        out.push(ShellInfo {
            id: "powershell".into(),
            label: "Windows PowerShell".into(),
            program: ps,
            // -NoLogo: the copyright banner wastes the first two rows of a
            // terminal every single launch.
            args: vec!["-NoLogo".into()],
            env: vec![],
        });
    }

    if let Some(pwsh) = on_path("pwsh.exe") {
        out.push(ShellInfo {
            id: "pwsh".into(),
            label: "PowerShell 7".into(),
            program: pwsh,
            args: vec!["-NoLogo".into()],
            env: vec![],
        });
    }

    // Git for Windows. The bundled bash is a full MSYS2 environment and is what
    // "Git Bash" actually launches.
    let git_bash = [
        r"C:\Program Files\Git\bin\bash.exe",
        r"C:\Program Files (x86)\Git\bin\bash.exe",
    ]
    .into_iter()
    .map(String::from)
    .chain(
        std::env::var("LOCALAPPDATA")
            .ok()
            .map(|l| format!(r"{l}\Programs\Git\bin\bash.exe")),
    )
    .find(|p| exists(p));

    if let Some(bash) = git_bash {
        out.push(ShellInfo {
            id: "gitbash".into(),
            label: "Git Bash".into(),
            program: bash,
            // --login runs the MSYS profile, which is what puts git, ssh, and
            // the coreutils on PATH. -i because this is interactive.
            args: vec!["--login".into(), "-i".into()],
            env: vec![
                // Without MSYSTEM the login profile cannot pick a prefix and
                // the prompt comes up without the MINGW64 environment.
                ("MSYSTEM".into(), "MINGW64".into()),
                // Start in the directory we launched from rather than $HOME —
                // `--login` would otherwise always cd to home.
                ("CHERE_INVOKING".into(), "1".into()),
            ],
        });
    }

    // MSYS2 standalone, if someone has it.
    for (id, label, path, msystem) in [
        (
            "msys2-ucrt64",
            "MSYS2 UCRT64",
            r"C:\msys64\usr\bin\bash.exe",
            "UCRT64",
        ),
        (
            "msys2-mingw64",
            "MSYS2 MINGW64",
            r"C:\msys64\usr\bin\bash.exe",
            "MINGW64",
        ),
    ] {
        if exists(path) && !out.iter().any(|s| s.id == id) {
            out.push(ShellInfo {
                id: id.into(),
                label: label.into(),
                program: path.into(),
                args: vec!["--login".into(), "-i".into()],
                env: vec![
                    ("MSYSTEM".into(), msystem.into()),
                    ("CHERE_INVOKING".into(), "1".into()),
                ],
            });
            break; // one MSYS2 entry is enough
        }
    }

    if let Some(wsl) = on_path("wsl.exe") {
        out.push(ShellInfo {
            id: "wsl".into(),
            label: "WSL (default distro)".into(),
            program: wsl,
            args: vec![],
            env: vec![],
        });
    }

    out
}

/// Entries in `/etc/shells` that are not something a person types into.
#[cfg(not(windows))]
const NOT_INTERACTIVE: &[&str] = &[
    "nologin", "false", "true", "git-shell", "tmux", "screen", "rbash", "rzsh", "rksh",
];

/// Unix shells: `$SHELL` first, then whatever `/etc/shells` lists.
///
/// `/etc/shells` is the system's own answer to "which shells are installed",
/// and it is the only list that is right on every distro: zsh is `/usr/bin/zsh`
/// on Debian, `/bin/zsh` on Arch, `/usr/local/bin/zsh` from Homebrew, and fish
/// lives in three places. Hardcoding paths gets one of those wrong.
///
/// Entries are deduplicated by **canonical path**. Since the `/usr` merge,
/// `/bin` is a symlink to `/usr/bin` on Debian, Fedora, Arch and Ubuntu, so
/// `/etc/shells` lists `/bin/bash` and `/usr/bin/bash` — the same binary. They
/// are also deduplicated by name, for the systems where they are two copies.
#[cfg(not(windows))]
fn candidates() -> Vec<ShellInfo> {
    let mut out: Vec<ShellInfo> = Vec::new();
    let mut seen_paths: Vec<std::path::PathBuf> = Vec::new();
    let mut seen_names: Vec<String> = Vec::new();

    let mut offer = |id: Option<&str>, label_suffix: &str, path: &str| {
        if !exists(path) {
            return;
        }
        let Some(name) = std::path::Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
        else {
            return;
        };
        // Applies to $SHELL too: a service account's is `nologin`.
        if NOT_INTERACTIVE.contains(&name.as_str()) {
            return;
        }
        let canonical =
            std::fs::canonicalize(path).unwrap_or_else(|_| std::path::PathBuf::from(path));
        if seen_paths.contains(&canonical) || seen_names.contains(&name) {
            return;
        }
        seen_paths.push(canonical);
        seen_names.push(name.clone());
        out.push(ShellInfo {
            id: id.map(String::from).unwrap_or_else(|| name.clone()),
            label: format!("{name}{label_suffix}"),
            program: path.into(),
            // Interactive, not login: what GNOME Terminal and Konsole do. A
            // login shell re-runs ~/.profile on every tab, which the desktop
            // session already did once.
            args: vec!["-i".into()],
            env: vec![],
        });
    };

    if let Ok(sh) = std::env::var("SHELL") {
        offer(Some("login"), " (default)", &sh);
    }

    let listed = std::fs::read_to_string("/etc/shells").unwrap_or_default();
    let from_file = listed
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with('/'));
    // Fallbacks for a system with no /etc/shells (minimal containers, NixOS
    // without the file populated). The dedup above makes them free otherwise.
    let fallbacks = ["/bin/bash", "/usr/bin/bash", "/bin/zsh", "/usr/bin/zsh", "/bin/sh"];
    for path in from_file.chain(fallbacks) {
        offer(None, "", path);
    }

    // `/etc/shells` is in install order, which puts `/bin/sh` first on Debian.
    // Keep the user's default on top, then the shells people actually pick.
    // Stable sort: anything unranked keeps its file order. O(n log n), n < 20.
    let rank = |s: &ShellInfo| match s.id.as_str() {
        "login" => 0,
        "bash" => 1,
        "zsh" => 2,
        "fish" => 3,
        "sh" | "dash" => 9,
        _ => 5,
    };
    out.sort_by_key(rank);
    out
}

/// Every shell present on this machine, best-default first.
pub fn available() -> Vec<ShellInfo> {
    candidates()
}

/// Resolve an id to a shell, falling back to the first available one.
pub fn resolve(id: Option<&str>) -> Option<ShellInfo> {
    let all = available();
    match id {
        Some(want) => all
            .iter()
            .find(|s| s.id == want)
            .cloned()
            .or_else(|| all.first().cloned()),
        None => all.first().cloned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn at_least_one_shell_exists_on_any_machine() {
        // If this fails the terminal cannot open at all, which is worth knowing
        // loudly rather than as a runtime error in the UI.
        assert!(!available().is_empty(), "no usable shell found");
    }

    #[test]
    fn every_offered_shell_actually_exists() {
        for s in available() {
            assert!(
                std::path::Path::new(&s.program).is_file(),
                "offered {} -> {} which is not a file",
                s.id,
                s.program
            );
        }
    }

    #[test]
    fn ids_are_unique() {
        let all = available();
        let mut ids: Vec<_> = all.iter().map(|s| s.id.clone()).collect();
        ids.sort();
        let before = ids.len();
        ids.dedup();
        assert_eq!(before, ids.len(), "duplicate shell ids");
    }

    /// usr-merged distros list `/bin/zsh` and `/usr/bin/zsh` in /etc/shells and
    /// often set `$SHELL` to one of them: three spellings, one binary, and a
    /// picker that offers "zsh" twice.
    #[cfg(unix)]
    #[test]
    fn no_binary_is_offered_twice() {
        let mut canon: Vec<_> = available()
            .iter()
            .map(|s| std::fs::canonicalize(&s.program).unwrap())
            .collect();
        let before = canon.len();
        canon.sort();
        canon.dedup();
        assert_eq!(before, canon.len(), "same shell offered under two paths");
    }

    #[cfg(unix)]
    #[test]
    fn non_interactive_entries_are_never_offered() {
        for s in available() {
            let name = s.program.rsplit('/').next().unwrap_or("");
            assert!(!NOT_INTERACTIVE.contains(&name), "offered {}", s.program);
        }
    }

    #[test]
    fn resolve_falls_back_rather_than_failing() {
        assert!(resolve(Some("definitely-not-a-shell")).is_some());
        assert!(resolve(None).is_some());
    }
}
