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

/// Look up a bare command name on `PATH`.
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

#[cfg(not(windows))]
fn candidates() -> Vec<ShellInfo> {
    let mut out = Vec::new();

    if let Ok(sh) = std::env::var("SHELL") {
        if exists(&sh) {
            let label = std::path::Path::new(&sh)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Login shell".into());
            out.push(ShellInfo {
                id: "login".into(),
                label: format!("{label} (login shell)"),
                program: sh,
                args: vec!["-i".into()],
                env: vec![],
            });
        }
    }

    for (id, label, path) in [
        ("bash", "bash", "/bin/bash"),
        ("zsh", "zsh", "/bin/zsh"),
        ("fish", "fish", "/usr/bin/fish"),
        ("sh", "sh", "/bin/sh"),
    ] {
        if exists(path) && !out.iter().any(|s| s.program == path) {
            out.push(ShellInfo {
                id: id.into(),
                label: label.into(),
                program: path.into(),
                args: vec!["-i".into()],
                env: vec![],
            });
        }
    }

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

    #[test]
    fn resolve_falls_back_rather_than_failing() {
        assert!(resolve(Some("definitely-not-a-shell")).is_some());
        assert!(resolve(None).is_some());
    }
}
