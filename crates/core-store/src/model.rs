//! The workspace: hosts, identities, and port forwards.
//!
//! CLAUDE.md §12: TOML is the source of truth, human-readable and diffable.
//! §12.1 puts hosts in `workspaces/*.toml` and identities in `identities.toml`
//! with "identities → vault refs. NO secrets."
//!
//! That last rule is honoured literally here: an [`Identity`] holds a *path to a
//! key* or a *vault reference*, never a password or a passphrase. Nothing in
//! this file can serialize a secret, because there is no field to put one in.

use serde::{Deserialize, Serialize};

/// Bumped when the on-disk shape changes incompatibly. §12.2: "Never silently
/// rewrite a config from a newer version than the binary understands — refuse
/// and say so."
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Environment {
    Prod,
    Stage,
    #[default]
    Dev,
    Test,
    Customer,
}

/// How to authenticate. Named, because §8.1 keys transports by identity and
/// §0.6 shows the name on every port chip (`SFTP·deploy`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub id: String,
    /// Shown on chips and in the SFTP pane header.
    pub name: String,
    pub user: String,
    /// Portable path to a private key — `~/.ssh/id_ed25519`, never absolute.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_path: Option<String>,
    /// A reference like `vault:prod/deploy-key`, resolved by the secrets broker
    /// at connect time. NEVER the secret itself (§12.1, §12.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_ref: Option<String>,
    /// The same, for the passphrase of an encrypted `key_path`. Separate from
    /// `password_ref` because they are different secrets: a key passphrase
    /// unlocks a local file, an account password authenticates to the server,
    /// and a host can need both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passphrase_ref: Option<String>,
    /// Read-only identities disable mutating SFTP controls rather than hiding
    /// them (§Step 10).
    #[serde(default)]
    pub read_only: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ForwardKind {
    /// `-L`: listen locally, connect from the server.
    Local,
    /// `-R`: listen on the server, connect from here.
    Remote,
    /// `-D`: local SOCKS proxy.
    Dynamic,
}

/// A saved port forward. Termius calls these "port forwarding rules".
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Forward {
    pub id: String,
    pub name: String,
    pub kind: ForwardKind,
    /// Address to bind locally (Local/Dynamic) or on the server (Remote).
    #[serde(default = "loopback")]
    pub bind_address: String,
    pub bind_port: u16,
    /// Where traffic is delivered. Unused for Dynamic, which decides per
    /// connection from the SOCKS handshake.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_port: Option<u16>,
    /// Start this forward as soon as its host connects.
    #[serde(default)]
    pub auto_start: bool,
}

fn loopback() -> String {
    // Binding 0.0.0.0 by default would expose a tunnel into production to the
    // whole local network. Anyone who wants that can type it.
    "127.0.0.1".to_string()
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Host {
    pub id: String,
    pub name: String,
    pub address: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub os: String,
    #[serde(default)]
    pub environment: Environment,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Identities usable on this host. The first is the default.
    #[serde(default)]
    pub identities: Vec<Identity>,
    #[serde(default)]
    pub forwards: Vec<Forward>,
    /// Optional jump host id — reserved; not yet honoured by the transport.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jump_host: Option<String>,
}

fn default_port() -> u16 {
    22
}

/// One workspace file — `workspaces/production.toml`.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Workspace {
    #[serde(default = "current_schema")]
    pub schema_version: u32,
    #[serde(default)]
    pub name: String,
    #[serde(default, rename = "host")]
    pub hosts: Vec<Host>,
}

fn current_schema() -> u32 {
    SCHEMA_VERSION
}

impl Workspace {
    pub fn new(name: &str) -> Workspace {
        Workspace {
            schema_version: SCHEMA_VERSION,
            name: name.to_string(),
            hosts: Vec::new(),
        }
    }

    /// §12.2: refuse a config from a newer binary rather than rewriting it.
    pub fn check_version(&self) -> Result<(), String> {
        if self.schema_version > SCHEMA_VERSION {
            return Err(format!(
                "this workspace was written by a newer OpsPilot (schema {} > {}). \
                 Refusing to open it, because saving would silently drop whatever \
                 that version added.",
                self.schema_version, SCHEMA_VERSION
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Workspace {
        Workspace {
            schema_version: SCHEMA_VERSION,
            name: "production".into(),
            hosts: vec![Host {
                id: "h1".into(),
                name: "linux-01".into(),
                address: "10.20.4.11".into(),
                port: 22,
                os: "ubuntu 22.04".into(),
                environment: Environment::Prod,
                tags: vec!["app".into()],
                identities: vec![Identity {
                    id: "i1".into(),
                    name: "deploy".into(),
                    user: "deploy".into(),
                    key_path: Some("~/.ssh/id_ed25519".into()),
                    password_ref: None,
                    passphrase_ref: None,
                    read_only: false,
                }],
                forwards: vec![Forward {
                    id: "f1".into(),
                    name: "postgres".into(),
                    kind: ForwardKind::Local,
                    bind_address: "127.0.0.1".into(),
                    bind_port: 15432,
                    target_host: Some("10.20.9.4".into()),
                    target_port: Some(5432),
                    auto_start: true,
                }],
                jump_host: None,
            }],
        }
    }

    #[test]
    fn round_trips_through_toml() {
        let ws = sample();
        let text = toml::to_string_pretty(&ws).unwrap();
        let back: Workspace = toml::from_str(&text).unwrap();
        assert_eq!(back.hosts, ws.hosts);
    }

    #[test]
    fn the_serialized_form_is_readable_and_holds_no_secrets() {
        let text = toml::to_string_pretty(&sample()).unwrap();
        assert!(
            text.contains("name = \"linux-01\""),
            "not human-readable:\n{text}"
        );
        // §12.1: "identities → vault refs. NO secrets." There is no field that
        // could carry one, and this test exists so adding one is a deliberate
        // act that breaks a test rather than a quiet mistake.
        let lower = text.to_lowercase();
        assert!(
            !lower.contains("password ="),
            "a secret reached the config:\n{text}"
        );
        // `passphrase_ref` is a legitimate field, so this checks for the
        // *assignment* rather than the word: `passphrase = ` is a secret,
        // `passphrase_ref = "vault:..."` is a pointer to one.
        assert!(
            !lower.contains("passphrase ="),
            "a secret reached the config:\n{text}"
        );
    }

    /// The key-passphrase reference must survive a round trip, and must not be
    /// confused with the password reference beside it.
    ///
    /// They are different secrets — one unlocks a local key file, the other
    /// authenticates to the server — and a host can need both. Swapping them
    /// silently would produce an auth failure with no clue as to why.
    #[test]
    fn a_passphrase_reference_round_trips_separately_from_the_password() {
        let mut ws = sample();
        ws.hosts[0].identities[0].password_ref = Some("vault:prod/deploy".into());
        ws.hosts[0].identities[0].passphrase_ref = Some("vault:prod/key".into());

        let text = toml::to_string_pretty(&ws).unwrap();
        assert!(text.contains("passphrase_ref = \"vault:prod/key\""), "{text}");
        // Still no plaintext field of either kind.
        assert!(!text.to_lowercase().contains("passphrase ="), "{text}");

        let back: Workspace = toml::from_str(&text).unwrap();
        let idn = &back.hosts[0].identities[0];
        assert_eq!(idn.passphrase_ref.as_deref(), Some("vault:prod/key"));
        assert_eq!(idn.password_ref.as_deref(), Some("vault:prod/deploy"));

        // And an identity without one must not gain an empty reference, or
        // every connect would try to resolve "".
        assert!(!toml::to_string_pretty(&sample())
            .unwrap()
            .contains("passphrase_ref"));
    }

    /// A vault reference must survive save and load.
    ///
    /// The whole point of §12.3's `password_ref` is that a saved host remembers
    /// which secret it uses. If serde ever dropped this field the app would
    /// silently fall back to prompting, which looks like a UI regression rather
    /// than a serialization bug and would be miserable to track down.
    ///
    /// It also checks the shape written to disk: `password_ref = "vault:..."`
    /// and never `password = ...`, so `the_serialized_form_is_readable_and_\
    /// holds_no_secrets` above cannot start passing for the wrong reason.
    #[test]
    fn a_vault_reference_round_trips_and_is_not_mistaken_for_a_password() {
        let mut ws = sample();
        ws.hosts[0].identities[0].password_ref = Some("vault:prod/deploy".into());

        let text = toml::to_string_pretty(&ws).unwrap();
        assert!(
            text.contains("password_ref = \"vault:prod/deploy\""),
            "the reference is not in the file:\n{text}"
        );
        // The reference is not a secret, but the field next to it would be.
        assert!(
            !text.to_lowercase().contains("\npassword ="),
            "a plain password field appeared:\n{text}"
        );

        let back: Workspace = toml::from_str(&text).unwrap();
        assert_eq!(
            back.hosts[0].identities[0].password_ref.as_deref(),
            Some("vault:prod/deploy"),
        );

        // And a host with no reference must not gain an empty one, or every
        // connect would try to resolve "".
        let plain = toml::to_string_pretty(&sample()).unwrap();
        assert!(!plain.contains("password_ref"), "{plain}");
    }

    #[test]
    fn a_newer_schema_is_refused_rather_than_rewritten() {
        let mut ws = sample();
        ws.schema_version = SCHEMA_VERSION + 1;
        let err = ws.check_version().unwrap_err();
        assert!(err.contains("newer OpsPilot"), "{err}");
    }

    #[test]
    fn defaults_fill_in_for_a_minimal_host_entry() {
        // Someone hand-editing hosts.toml in Vim should not need every field.
        let text = r#"
            name = "scratch"
            [[host]]
            id = "x"
            name = "box"
            address = "192.168.1.5"
        "#;
        let ws: Workspace = toml::from_str(text).unwrap();
        let h = &ws.hosts[0];
        assert_eq!(h.port, 22);
        assert_eq!(h.environment, Environment::Dev);
        assert!(h.identities.is_empty());
    }

    #[test]
    fn forwards_bind_loopback_unless_told_otherwise() {
        // Defaulting to 0.0.0.0 would expose a tunnel into production to the
        // whole local network.
        let text = r#"
            name = "w"
            [[host]]
            id = "x"
            name = "b"
            address = "a"
            [[host.forwards]]
            id = "f"
            name = "pg"
            kind = "local"
            bind_port = 15432
        "#;
        let ws: Workspace = toml::from_str(text).unwrap();
        assert_eq!(ws.hosts[0].forwards[0].bind_address, "127.0.0.1");
    }
}
