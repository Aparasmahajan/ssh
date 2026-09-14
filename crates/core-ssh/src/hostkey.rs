//! Host key verification.
//!
//! This is the security boundary of the whole SSH feature, so it is its own
//! module with its own tests. russh's `Handler::check_server_key` **defaults to
//! rejecting everything**, which is the right default and the reason a wrong
//! implementation here is a silent downgrade rather than a visible failure.
//!
//! The rule that does not bend: **a changed host key is always rejected**, under
//! every policy. Unknown-and-new is a judgement call about trust-on-first-use;
//! changed is either a reinstalled server or a machine-in-the-middle, and a tool
//! that guesses between those on the user's behalf is doing harm.
//!
//! `known_hosts` lives in the portable config directory (CLAUDE.md §12.1:
//! "known_hosts — portable — host keys belong to servers").

use std::path::{Path, PathBuf};

use russh::keys::known_hosts::{check_known_hosts_path, learn_known_hosts_path};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostKeyPolicy {
    /// The key must already be in `known_hosts`. Anything else fails.
    Strict,
    /// Trust on first use: an unknown host is recorded and accepted. A *changed*
    /// key is still rejected.
    AcceptNew,
    /// Verify, but never write. For probes and speculative connects (§8.2),
    /// where recording a key the user never agreed to connect to is wrong.
    VerifyOnly,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Key is recorded and matches.
    Known,
    /// Host was unknown and has now been recorded (`AcceptNew` only).
    Learned,
    /// Host is unknown and policy would not record it.
    Unknown,
    /// Recorded key differs. Never acceptable.
    Changed { line: usize },
    /// The known_hosts file could not be read.
    Error(String),
}

impl Verdict {
    pub fn accepted(&self) -> bool {
        matches!(self, Verdict::Known | Verdict::Learned)
    }

    /// A message fit to show a user. `Changed` deliberately reads like the
    /// OpenSSH warning, because users have been trained to take that seriously.
    pub fn message(&self, host: &str) -> String {
        match self {
            Verdict::Known => format!("{host}: host key verified"),
            Verdict::Learned => format!("{host}: new host key recorded in known_hosts"),
            Verdict::Unknown => {
                format!("{host}: host key is not in known_hosts and the policy will not add it")
            }
            Verdict::Changed { line } => format!(
                "REMOTE HOST IDENTIFICATION HAS CHANGED for {host}. \
                 The key on line {line} of known_hosts does not match the key the server \
                 presented. This is either a reinstalled server or a machine-in-the-middle. \
                 Refusing to connect."
            ),
            Verdict::Error(e) => format!("{host}: could not check known_hosts: {e}"),
        }
    }
}

/// Default location. Portable config dir per §12.1, overridable by
/// `OPSPILOT_CONFIG_DIR` (§12.1: "so a fully portable USB install is just an env
/// var").
pub fn default_known_hosts() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("OPSPILOT_CONFIG_DIR") {
        return Some(PathBuf::from(dir).join("known_hosts"));
    }
    #[allow(deprecated)]
    std::env::home_dir().map(|h| h.join(".ssh").join("known_hosts"))
}

/// Check a presented key against `known_hosts`, applying `policy`.
pub fn verify(
    host: &str,
    port: u16,
    key: &russh::keys::ssh_key::PublicKey,
    path: &Path,
    policy: HostKeyPolicy,
) -> Verdict {
    match check_known_hosts_path(host, port, key, path) {
        Ok(true) => Verdict::Known,
        Ok(false) => match policy {
            HostKeyPolicy::AcceptNew => match learn_known_hosts_path(host, port, key, path) {
                Ok(()) => Verdict::Learned,
                Err(e) => Verdict::Error(e.to_string()),
            },
            HostKeyPolicy::Strict | HostKeyPolicy::VerifyOnly => Verdict::Unknown,
        },
        // A changed key is rejected under every policy, including AcceptNew.
        Err(russh::keys::Error::KeyChanged { line }) => Verdict::Changed { line },
        Err(e) => {
            // A missing known_hosts is "unknown host", not an error: on a fresh
            // install the file does not exist yet and AcceptNew must still work.
            if !path.exists() {
                return match policy {
                    HostKeyPolicy::AcceptNew => match learn_known_hosts_path(host, port, key, path)
                    {
                        Ok(()) => Verdict::Learned,
                        Err(e) => Verdict::Error(e.to_string()),
                    },
                    _ => Verdict::Unknown,
                };
            }
            Verdict::Error(e.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh::keys::ssh_key::PublicKey;

    /// Two throwaway ed25519 public keys, generated once with `ssh-keygen` and
    /// pasted here. Fixtures rather than freshly generated keys: no RNG
    /// dependency, and byte-for-byte determinism, which matters because the
    /// property under test is exact key comparison. Public halves only — there is
    /// no secret material in this file.
    ///
    /// **No trailing comment, deliberately.** `ssh-key`'s `PublicKey` equality
    /// includes the comment field, and `learn_known_hosts_path` round-trips the
    /// key through `to_openssh()`. A fixture carrying `opspilot-test-a` therefore
    /// compared unequal to its own recorded copy and reported a *changed host
    /// key* — which is what a server-key rotation looks like, so a wrong fixture
    /// here would have looked like a real security finding. Server host keys
    /// carry no comment, so this also makes the fixtures faithful.
    const KEY_A: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIM8/ctlGrIlXSE2g9Woy8UNSHhmDPKrYUzTt4LnGq7zg";
    const KEY_B: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIB/sNgFpPWsHhN2B66z/1GbaBhLy/SCJfwdHWIaj8yQ3";

    fn key_a() -> PublicKey {
        PublicKey::from_openssh(KEY_A).unwrap()
    }
    fn key_b() -> PublicKey {
        PublicKey::from_openssh(KEY_B).unwrap()
    }

    /// Per-test directory, so the four tests cannot see each other's
    /// known_hosts. They each write to one, and a shared file would make the
    /// results order-dependent.
    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("opspilot-hk-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn unknown_host_is_learned_under_accept_new_and_refused_under_strict() {
        let dir = tmpdir("learn");
        let strict_path = dir.join("kh_strict");
        let accept_path = dir.join("kh_accept");
        let _ = std::fs::remove_file(&strict_path);
        let _ = std::fs::remove_file(&accept_path);

        let pubkey = &key_a();

        assert_eq!(
            verify(
                "h1.example",
                22,
                pubkey,
                &strict_path,
                HostKeyPolicy::Strict
            ),
            Verdict::Unknown
        );
        assert!(
            !strict_path.exists(),
            "Strict must never write to known_hosts"
        );

        assert_eq!(
            verify(
                "h1.example",
                22,
                pubkey,
                &accept_path,
                HostKeyPolicy::AcceptNew
            ),
            Verdict::Learned
        );
        // Second time it is simply known.
        assert_eq!(
            verify(
                "h1.example",
                22,
                pubkey,
                &accept_path,
                HostKeyPolicy::AcceptNew
            ),
            Verdict::Known
        );
    }

    #[test]
    fn a_changed_key_is_rejected_even_under_accept_new() {
        // The property that matters most in this file.
        let dir = tmpdir("changed");
        let path = dir.join("kh_changed");
        let _ = std::fs::remove_file(&path);

        let original = key_a();
        let impostor = key_b();
        assert_ne!(original, impostor);

        assert_eq!(
            verify("h2.example", 22, &original, &path, HostKeyPolicy::AcceptNew),
            Verdict::Learned
        );

        let verdict = verify("h2.example", 22, &impostor, &path, HostKeyPolicy::AcceptNew);
        assert!(
            matches!(verdict, Verdict::Changed { .. }),
            "expected Changed, got {verdict:?}"
        );
        assert!(!verdict.accepted());
        assert!(verdict.message("h2.example").contains("HAS CHANGED"));
    }

    #[test]
    fn verify_only_never_writes() {
        let dir = tmpdir("verifyonly");
        let path = dir.join("kh_vo");
        let _ = std::fs::remove_file(&path);
        let key = key_a();
        assert_eq!(
            verify("h3.example", 22, &key, &path, HostKeyPolicy::VerifyOnly),
            Verdict::Unknown
        );
        assert!(!path.exists());
    }

    #[test]
    fn different_ports_are_different_hosts() {
        let dir = tmpdir("ports");
        let path = dir.join("kh_ports");
        let _ = std::fs::remove_file(&path);
        let key = key_a();
        assert_eq!(
            verify("h4.example", 22, &key, &path, HostKeyPolicy::AcceptNew),
            Verdict::Learned
        );
        assert_eq!(
            verify("h4.example", 2222, &key, &path, HostKeyPolicy::Strict),
            Verdict::Unknown,
            "a key learned for :22 must not authorise :2222"
        );
    }

    #[test]
    fn only_accepted_verdicts_pass() {
        assert!(Verdict::Known.accepted());
        assert!(Verdict::Learned.accepted());
        assert!(!Verdict::Unknown.accepted());
        assert!(!Verdict::Changed { line: 1 }.accepted());
        assert!(!Verdict::Error("x".into()).accepted());
    }
}
