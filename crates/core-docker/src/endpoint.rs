//! Which Docker daemon to talk to — resolved the way the `docker` CLI does.
//!
//! The obvious implementation, "`DOCKER_HOST` or `/var/run/docker.sock`", is
//! what bollard's `connect_with_local_defaults` does, and it is wrong on any
//! machine running **Docker Desktop for Linux** or rootless Docker: there the
//! CLI talks to `~/.docker/desktop/docker.sock` (or `$XDG_RUNTIME_DIR/docker.sock`)
//! through a *context*, and `/var/run/docker.sock` does not exist at all. The
//! user's `docker ps` works and the app says "Docker not running". Found on the
//! development machine, which is exactly that setup.
//!
//! Precedence, matching the CLI:
//!   1. `DOCKER_HOST`
//!   2. `DOCKER_CONTEXT`, else `currentContext` in `~/.docker/config.json`
//!      (`$DOCKER_CONFIG` overrides the directory), resolved through
//!      `contexts/meta/<sha256(name)>/meta.json`
//!   3. the platform default socket.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    /// `unix:///path`, `tcp://host:port`, or `npipe:////./pipe/name`.
    pub host: String,
    /// Where it came from, shown in the UI so a wrong daemon is diagnosable:
    /// "context desktop-linux", "DOCKER_HOST", "default".
    pub source: String,
}

#[cfg(windows)]
const DEFAULT_HOST: &str = "npipe:////./pipe/docker_engine";
#[cfg(not(windows))]
const DEFAULT_HOST: &str = "unix:///var/run/docker.sock";

/// Everything resolution reads, so it is testable without touching the real
/// environment or home directory (CLAUDE.md §18.10: env-var tests lie when run
/// in parallel).
pub struct Env<'a> {
    pub docker_host: Option<&'a str>,
    pub docker_context: Option<&'a str>,
    /// The Docker config directory: `$DOCKER_CONFIG` or `~/.docker`.
    pub config_dir: Option<&'a Path>,
}

pub fn resolve_from(env: &Env<'_>) -> Result<Endpoint, String> {
    if let Some(h) = env.docker_host.filter(|h| !h.is_empty()) {
        return Ok(Endpoint {
            host: h.to_string(),
            source: "DOCKER_HOST".into(),
        });
    }

    let context = match env.docker_context.filter(|c| !c.is_empty()) {
        Some(c) => Some(c.to_string()),
        None => env.config_dir.and_then(current_context),
    };

    match context.as_deref() {
        // "default" is the CLI's name for "no context": the platform socket.
        None | Some("default") => Ok(Endpoint {
            host: DEFAULT_HOST.into(),
            source: "default".into(),
        }),
        Some(name) => {
            let dir = env
                .config_dir
                .ok_or_else(|| format!("docker context `{name}` set but no config directory"))?;
            let host = context_host(dir, name)?;
            Ok(Endpoint {
                host,
                source: format!("context {name}"),
            })
        }
    }
}

/// Resolve against the real process environment.
pub fn resolve() -> Result<Endpoint, String> {
    let host = std::env::var("DOCKER_HOST").ok();
    let ctx = std::env::var("DOCKER_CONTEXT").ok();
    let dir: Option<PathBuf> = std::env::var_os("DOCKER_CONFIG")
        .map(PathBuf::from)
        .or_else(|| std::env::home_dir().map(|h| h.join(".docker")));
    resolve_from(&Env {
        docker_host: host.as_deref(),
        docker_context: ctx.as_deref(),
        config_dir: dir.as_deref(),
    })
}

fn current_context(config_dir: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(config_dir.join("config.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    v.get("currentContext")?.as_str().map(str::to_owned)
}

fn context_host(config_dir: &Path, name: &str) -> Result<String, String> {
    let digest = Sha256::digest(name.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    let meta = config_dir
        .join("contexts")
        .join("meta")
        .join(hex)
        .join("meta.json");
    let raw = std::fs::read_to_string(&meta)
        .map_err(|e| format!("docker context `{name}`: cannot read {}: {e}", meta.display()))?;
    let v: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("docker context `{name}`: {e}"))?;
    v.pointer("/Endpoints/docker/Host")
        .and_then(|h| h.as_str())
        .map(str::to_owned)
        .ok_or_else(|| format!("docker context `{name}` has no docker endpoint"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("opspilot-docker-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write_context(dir: &Path, name: &str, host: &str) {
        let hex: String = Sha256::digest(name.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let d = dir.join("contexts/meta").join(hex);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("meta.json"),
            format!(r#"{{"Name":"{name}","Endpoints":{{"docker":{{"Host":"{host}"}}}}}}"#),
        )
        .unwrap();
    }

    #[test]
    fn docker_host_wins_over_everything() {
        let e = resolve_from(&Env {
            docker_host: Some("tcp://10.0.0.5:2375"),
            docker_context: Some("x"),
            config_dir: None,
        })
        .unwrap();
        assert_eq!(e.host, "tcp://10.0.0.5:2375");
        assert_eq!(e.source, "DOCKER_HOST");
    }

    #[test]
    fn docker_desktop_current_context_is_followed() {
        // The development machine's actual layout.
        let d = scratch("desktop");
        std::fs::write(d.join("config.json"), r#"{"auths":{},"currentContext":"desktop-linux"}"#)
            .unwrap();
        write_context(&d, "desktop-linux", "unix:///home/u/.docker/desktop/docker.sock");
        let e = resolve_from(&Env {
            docker_host: None,
            docker_context: None,
            config_dir: Some(&d),
        })
        .unwrap();
        assert_eq!(e.host, "unix:///home/u/.docker/desktop/docker.sock");
        assert_eq!(e.source, "context desktop-linux");
    }

    #[test]
    fn docker_context_env_overrides_the_config_file() {
        let d = scratch("ctxenv");
        std::fs::write(d.join("config.json"), r#"{"currentContext":"a"}"#).unwrap();
        write_context(&d, "a", "unix:///a.sock");
        write_context(&d, "b", "unix:///b.sock");
        let e = resolve_from(&Env {
            docker_host: None,
            docker_context: Some("b"),
            config_dir: Some(&d),
        })
        .unwrap();
        assert_eq!(e.host, "unix:///b.sock");
    }

    #[test]
    fn no_config_or_the_default_context_means_the_platform_socket() {
        let d = scratch("none");
        let e = resolve_from(&Env {
            docker_host: None,
            docker_context: None,
            config_dir: Some(&d),
        })
        .unwrap();
        assert_eq!(e.host, DEFAULT_HOST);
        std::fs::write(d.join("config.json"), r#"{"currentContext":"default"}"#).unwrap();
        let e = resolve_from(&Env {
            docker_host: None,
            docker_context: None,
            config_dir: Some(&d),
        })
        .unwrap();
        assert_eq!(e.host, DEFAULT_HOST);
    }

    #[test]
    fn a_missing_context_is_an_error_that_names_it() {
        let d = scratch("missing");
        let err = resolve_from(&Env {
            docker_host: None,
            docker_context: Some("gone"),
            config_dir: Some(&d),
        })
        .unwrap_err();
        assert!(err.contains("`gone`"), "{err}");
    }
}
