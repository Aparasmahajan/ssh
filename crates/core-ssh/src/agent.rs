//! SSH agent authentication.
//!
//! Agent auth is what most people actually use, and it has a property nothing
//! else here has: **the private key never leaves the agent**. OpsPilot asks the
//! agent to sign a challenge and forwards the signature. There is no key file to
//! read, no passphrase to hold, and nothing for §12.3's vault to protect —
//! which makes this the *least* dangerous credential path in the app, not just a
//! convenience.
//!
//! It is also the only method that can succeed with no prompt at all, so it is
//! the only one §8.2's speculative connect could ever be permitted to use
//! ("Never speculatively connect when auth would prompt the user").
//!
//! ## Where the agent lives
//!
//! | Platform | Transport | Set by |
//! |---|---|---|
//! | Unix | Unix socket | `SSH_AUTH_SOCK` |
//! | Windows | named pipe `\\.\pipe\openssh-ssh-agent` | OpenSSH for Windows |
//! | Windows | Pageant | PuTTY, WinSCP, KiTTY |
//!
//! Both Windows transports are tried, in that order, because a machine can
//! easily have both: OpenSSH ships with Windows and Pageant arrives with PuTTY.
//! Preferring the OpenSSH pipe matches what `ssh.exe` on the same machine does,
//! so a host that works in a terminal works here.

use russh::client::Handle;
use russh::keys::agent::client::{AgentClient, AgentStream};
use russh::keys::agent::AgentIdentity;
use tokio::io::{AsyncRead, AsyncWrite};

/// A stuck named pipe must not hang the connect.
///
/// `connect_named_pipe` retries `ERROR_PIPE_BUSY` in an unbounded loop. That is
/// reasonable for a pipe that is about to free up and unacceptable for one that
/// never will, so the whole connect attempt is bounded here instead.
#[cfg(windows)]
const AGENT_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Any agent connection, whatever transport it came over.
type Agent = AgentClient<Box<dyn AgentStream + Send + Unpin>>;

/// Find and connect to the running agent: `Err` with a reason if there is none.
async fn connect() -> Result<Agent, String> {
    #[cfg(unix)]
    {
        AgentClient::connect_env()
            .await
            .map(AgentClient::dynamic)
            .map_err(|e| format!("no SSH agent (SSH_AUTH_SOCK): {e}"))
    }

    #[cfg(windows)]
    {
        // `SSH_AUTH_SOCK` first: someone who has set it has said where their
        // agent is, and guessing past that would be rude.
        let explicit = std::env::var("SSH_AUTH_SOCK").ok().filter(|s| !s.is_empty());
        let pipe = explicit.unwrap_or_else(|| r"\\.\pipe\openssh-ssh-agent".to_string());

        let openssh = tokio::time::timeout(
            AGENT_CONNECT_TIMEOUT,
            AgentClient::connect_named_pipe(pipe.clone()),
        )
        .await;
        if let Ok(Ok(agent)) = openssh {
            return Ok(agent.dynamic());
        }

        // Pageant, for the PuTTY/WinSCP half of the world.
        match tokio::time::timeout(AGENT_CONNECT_TIMEOUT, AgentClient::connect_pageant()).await {
            Ok(Ok(agent)) => Ok(agent.dynamic()),
            Ok(Err(e)) => Err(format!("no SSH agent ({pipe} or Pageant): {e}")),
            Err(_) => Err(format!("SSH agent did not respond ({pipe} or Pageant)")),
        }
    }

    #[cfg(not(any(unix, windows)))]
    {
        Err("SSH agent support is not implemented on this platform".into())
    }
}

/// Try every identity the agent holds. `Ok(true)` on success, `Ok(false)` if the
/// agent answered but the server rejected all of its keys, `Err` with a reason
/// if there is no usable agent at all.
pub(crate) async fn authenticate<H: russh::client::Handler>(
    handle: &mut Handle<H>,
    user: &str,
) -> Result<bool, String> {
    let agent = connect().await?;
    try_identities(handle, user, agent).await
}

/// What the agent can offer, for the connect dialog. Public keys only: the
/// agent never hands out anything else, so there is nothing secret here.
#[derive(Clone, Debug, Default)]
pub struct AgentStatus {
    /// An agent answered.
    pub available: bool,
    /// One line per usable key: `ED25519 SHA256:… comment`.
    pub keys: Vec<String>,
    /// Why there is no agent, or why it would not list its keys.
    pub error: Option<String>,
}

/// Ask the running agent what it holds. Never fails: "no agent" is a status.
pub async fn status() -> AgentStatus {
    let mut agent = match connect().await {
        Ok(a) => a,
        Err(e) => {
            return AgentStatus {
                error: Some(e),
                ..Default::default()
            }
        }
    };
    match agent.request_identities().await {
        Ok(ids) => AgentStatus {
            available: true,
            keys: ids
                .iter()
                .filter_map(|id| match id {
                    // Certificates are not offered by `try_identities`, so
                    // listing them would promise something that is not tried.
                    AgentIdentity::PublicKey { key, comment } => Some(format!(
                        "{} {} {}",
                        key.algorithm(),
                        key.fingerprint(Default::default()),
                        comment
                    )
                    .trim_end()
                    .to_string()),
                    _ => None,
                })
                .collect(),
            error: None,
        },
        Err(e) => AgentStatus {
            available: true,
            keys: Vec::new(),
            error: Some(format!("the SSH agent would not list its keys: {e}")),
        },
    }
}

/// Offer each of the agent's public keys in turn.
///
/// Certificates are skipped rather than mishandled: they authenticate through a
/// different russh call, and offering the certificate's embedded public key as
/// though it were a plain key is not the same request.
async fn try_identities<H, S>(
    handle: &mut Handle<H>,
    user: &str,
    mut agent: AgentClient<S>,
) -> Result<bool, String>
where
    H: russh::client::Handler,
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let identities = agent
        .request_identities()
        .await
        .map_err(|e| format!("the SSH agent would not list its keys: {e}"))?;

    if identities.is_empty() {
        return Err("the SSH agent is running but holds no keys (ssh-add?)".into());
    }

    // Ask once, not per key: it is a round trip, and the answer is a property of
    // the server.
    let rsa_hash = handle
        .best_supported_rsa_hash()
        .await
        .map_err(|e| e.to_string())?
        .flatten();

    for identity in identities {
        let AgentIdentity::PublicKey { key, .. } = identity else {
            continue;
        };
        let result = handle
            .authenticate_publickey_with(user, key, rsa_hash, &mut agent)
            .await
            .map_err(|e| e.to_string())?;
        if result.success() {
            return Ok(true);
        }
    }
    Ok(false)
}
