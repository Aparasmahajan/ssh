//! Loading and saving workspaces.
//!
//! §12.2: TOML, atomic writes, `schema_version`, and hot reload — "Editing
//! `production.toml` in Vim makes hosts appear in the tree. Termius makes you
//! click through dialogs; this is a real differentiator and it costs almost
//! nothing."

use std::path::PathBuf;

use crate::model::{Host, Workspace, SCHEMA_VERSION};
use crate::paths::{config_dir, write_atomic};

fn workspaces_dir() -> PathBuf {
    config_dir().join("workspaces")
}

fn workspace_path(name: &str) -> PathBuf {
    // A workspace name becomes a filename, so it must not be able to escape the
    // directory. `../../etc/passwd` as a workspace name is not a threat model
    // worth ignoring just because the user typed it.
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    workspaces_dir().join(format!("{safe}.toml"))
}

/// Every workspace file present, by name.
pub fn list_workspaces() -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(workspaces_dir()) else {
        return Vec::new();
    };
    let mut out: Vec<String> = rd
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("toml") {
                return None;
            }
            p.file_stem().map(|s| s.to_string_lossy().into_owned())
        })
        .collect();
    out.sort();
    out
}

pub fn load(name: &str) -> Result<Workspace, String> {
    let path = workspace_path(name);
    if !path.exists() {
        return Ok(Workspace::new(name));
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut ws: Workspace =
        toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    ws.check_version()?;
    if ws.name.is_empty() {
        ws.name = name.to_string();
    }
    Ok(ws)
}

pub fn save(ws: &Workspace) -> Result<(), String> {
    let mut out = ws.clone();
    out.schema_version = SCHEMA_VERSION;
    let text = toml::to_string_pretty(&out).map_err(|e| e.to_string())?;
    let header = "# OpsPilot workspace — edit freely, the app reloads on change.\n\
                  # Paths are portable: use ~/... or ${env:VAR}, never an absolute path.\n\n";
    write_atomic(&workspace_path(&ws.name), &(header.to_string() + &text))
        .map_err(|e| format!("saving {}: {e}", ws.name))
}

/// Insert or replace a host by id.
pub fn upsert_host(workspace: &str, host: Host) -> Result<Workspace, String> {
    let mut ws = load(workspace)?;
    match ws.hosts.iter_mut().find(|h| h.id == host.id) {
        Some(existing) => *existing = host,
        None => ws.hosts.push(host),
    }
    save(&ws)?;
    Ok(ws)
}

pub fn delete_host(workspace: &str, host_id: &str) -> Result<Workspace, String> {
    let mut ws = load(workspace)?;
    ws.hosts.retain(|h| h.id != host_id);
    save(&ws)?;
    Ok(ws)
}

/// A workspace seeded from the machine's own `~/.ssh/config`-adjacent reality:
/// nothing invented, just an empty workspace with a helpful name.
pub fn ensure_default() -> Result<Workspace, String> {
    let names = list_workspaces();
    if let Some(first) = names.first() {
        return load(first);
    }
    let ws = Workspace::new("default");
    save(&ws)?;
    Ok(ws)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Environment, Identity};

    fn isolate(tag: &str) -> (PathBuf, std::sync::MutexGuard<'static, ()>) {
        let guard = crate::testlock::lock();
        let d = std::env::temp_dir().join(format!("opspilot-store-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::env::set_var("OPSPILOT_CONFIG_DIR", &d);
        (d, guard)
    }

    fn host(id: &str, name: &str) -> Host {
        Host {
            id: id.into(),
            name: name.into(),
            address: "10.0.0.1".into(),
            port: 22,
            os: String::new(),
            environment: Environment::Prod,
            tags: vec![],
            identities: vec![Identity {
                id: "i".into(),
                name: "deploy".into(),
                user: "deploy".into(),
                key_path: Some("~/.ssh/id_ed25519".into()),
                password_ref: None,
                passphrase_ref: None,
                read_only: false,
            }],
            forwards: vec![],
            jump_host: None,
        }
    }

    #[test]
    fn hosts_survive_a_save_and_load() {
        let _iso = isolate("roundtrip");
        upsert_host("wk", host("a", "linux-01")).unwrap();
        let ws = load("wk").unwrap();
        assert_eq!(ws.hosts.len(), 1);
        assert_eq!(ws.hosts[0].name, "linux-01");
        assert_eq!(
            ws.hosts[0].identities[0].key_path.as_deref(),
            Some("~/.ssh/id_ed25519")
        );
    }

    #[test]
    fn upsert_replaces_rather_than_duplicating() {
        let _iso = isolate("upsert");
        upsert_host("wk", host("a", "first")).unwrap();
        let ws = upsert_host("wk", host("a", "renamed")).unwrap();
        assert_eq!(ws.hosts.len(), 1);
        assert_eq!(ws.hosts[0].name, "renamed");
    }

    #[test]
    fn delete_removes_only_the_named_host() {
        let _iso = isolate("delete");
        upsert_host("wk", host("a", "one")).unwrap();
        upsert_host("wk", host("b", "two")).unwrap();
        let ws = delete_host("wk", "a").unwrap();
        assert_eq!(ws.hosts.len(), 1);
        assert_eq!(ws.hosts[0].id, "b");
    }

    #[test]
    fn the_file_on_disk_is_editable_by_hand() {
        // §12.2's hot-reload differentiator only works if the file is pleasant
        // to edit, so assert it actually looks like config and not like a dump.
        let (dir, _guard) = isolate("readable");
        upsert_host("wk", host("a", "linux-01")).unwrap();
        let text = std::fs::read_to_string(dir.join("workspaces").join("wk.toml")).unwrap();
        assert!(
            text.starts_with("# OpsPilot workspace"),
            "no header:\n{text}"
        );
        assert!(text.contains("[[host]]"), "not a host array:\n{text}");
        assert!(text.contains("name = \"linux-01\""), "{text}");
    }

    #[test]
    fn a_workspace_name_cannot_escape_the_directory() {
        let _iso = isolate("traversal");
        let p = workspace_path("../../evil");
        assert!(
            p.starts_with(workspaces_dir()),
            "path traversal via workspace name: {}",
            p.display()
        );
        assert!(!p.to_string_lossy().contains(".."));
    }

    #[test]
    fn a_missing_workspace_loads_as_empty_rather_than_failing() {
        let _iso = isolate("missing");
        let ws = load("never-created").unwrap();
        assert!(ws.hosts.is_empty());
        assert_eq!(ws.name, "never-created");
    }
}
