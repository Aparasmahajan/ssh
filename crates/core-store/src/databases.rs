//! Saved database connections — `databases.toml` in the portable config
//! (CLAUDE.md §12.1).
//!
//! ```toml
//! [[connection]]
//! id = "orders-prod"
//! name = "orders (prod)"
//! engine = "postgres"
//! host = "db.internal"
//! port = 5432
//! user = "readonly"
//! database = "orders"
//! password_ref = "vault:prod/orders-ro"
//! ```
//!
//! There is **no password field**. Not "optional", not "discouraged": absent, so
//! no code path can write one here. A password is either a vault reference,
//! resolved natively at connect time, or typed for that one connect.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::paths::{config_dir, portable, write_atomic};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct DbProfile {
    pub id: String,
    pub name: String,
    /// "postgres" | "mysql" | "sqlite"
    pub engine: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub host: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub port: u16,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub user: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub database: String,
    /// `vault:…` — never the secret itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_ref: Option<String>,
    /// SQLite file. Stored portable (`~/…`), resolved at connect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

fn is_zero(p: &u16) -> bool {
    *p == 0
}

#[derive(Serialize, Deserialize, Default)]
struct File {
    #[serde(default, rename = "connection")]
    connections: Vec<DbProfile>,
}

fn file_path() -> PathBuf {
    config_dir().join("databases.toml")
}

pub fn load() -> Result<Vec<DbProfile>, String> {
    let p = file_path();
    match std::fs::read_to_string(&p) {
        Ok(text) => toml::from_str::<File>(&text)
            .map(|f| f.connections)
            .map_err(|e| format!("{}: {e}", p.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("{}: {e}", p.display())),
    }
}

fn save(all: &[DbProfile]) -> Result<(), String> {
    let text = toml::to_string_pretty(&File {
        connections: all.to_vec(),
    })
    .map_err(|e| e.to_string())?;
    std::fs::create_dir_all(config_dir()).map_err(|e| e.to_string())?;
    write_atomic(&file_path(), &text).map_err(|e| e.to_string())
}

/// Ids are user-visible keys in a hand-editable file; keep them boring.
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

pub fn upsert(mut p: DbProfile) -> Result<Vec<DbProfile>, String> {
    if !valid_id(&p.id) {
        return Err(format!("invalid connection id `{}`: use letters, digits, - _ .", p.id));
    }
    if !matches!(p.engine.as_str(), "postgres" | "mysql" | "sqlite") {
        return Err(format!("unknown engine `{}`", p.engine));
    }
    if let Some(r) = &p.password_ref {
        // A plaintext password typed into the reference field must not be
        // written to a config file that is meant to be synced and committed.
        if !r.is_empty() && !r.starts_with("vault:") {
            return Err("password_ref must be a vault reference like vault:prod/db".into());
        }
    }
    p.password_ref = p.password_ref.filter(|r| !r.is_empty());
    // §12.2: no absolute home paths in portable config.
    p.path = p.path.filter(|s| !s.is_empty()).map(|s| portable(&s));
    let mut all = load()?;
    match all.iter_mut().find(|x| x.id == p.id) {
        Some(slot) => *slot = p,
        None => all.push(p),
    }
    save(&all)?;
    Ok(all)
}

pub fn delete(id: &str) -> Result<Vec<DbProfile>, String> {
    let mut all = load()?;
    all.retain(|x| x.id != id);
    save(&all)?;
    Ok(all)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn isolate(tag: &str) -> std::sync::MutexGuard<'static, ()> {
        let g = crate::testlock::lock();
        let d = std::env::temp_dir().join(format!("opspilot-db-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::env::set_var("OPSPILOT_CONFIG_DIR", &d);
        g
    }

    fn pg(id: &str) -> DbProfile {
        DbProfile {
            id: id.into(),
            name: "orders".into(),
            engine: "postgres".into(),
            host: "db".into(),
            port: 5432,
            user: "ro".into(),
            database: "orders".into(),
            password_ref: Some("vault:prod/orders".into()),
            path: None,
        }
    }

    #[test]
    fn round_trips_and_replaces_by_id() {
        let _g = isolate("rt");
        assert!(load().unwrap().is_empty());
        upsert(pg("a")).unwrap();
        let mut b = pg("a");
        b.name = "renamed".into();
        let all = upsert(b).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(load().unwrap()[0].name, "renamed");
        assert!(delete("a").unwrap().is_empty());
    }

    #[test]
    fn a_plaintext_password_is_refused() {
        let _g = isolate("pw");
        let mut p = pg("x");
        p.password_ref = Some("hunter2".into());
        assert!(upsert(p).is_err());
        assert!(!file_path().exists(), "nothing may be written");
    }

    #[test]
    fn sqlite_paths_are_stored_portable() {
        let _g = isolate("path");
        let home = dirs::home_dir().unwrap();
        let mut p = pg("lite");
        p.engine = "sqlite".into();
        p.path = Some(home.join("data/app.db").to_string_lossy().into_owned());
        upsert(p).unwrap();
        let text = std::fs::read_to_string(file_path()).unwrap();
        assert!(text.contains("~/data/app.db"), "{text}");
        assert!(!text.contains(&*home.to_string_lossy()), "absolute home path leaked: {text}");
    }

    #[test]
    fn bad_ids_and_engines_are_rejected() {
        let _g = isolate("bad");
        assert!(upsert(pg("../x")).is_err());
        let mut p = pg("ok");
        p.engine = "oracle".into();
        assert!(upsert(p).is_err());
    }
}
