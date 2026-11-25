//! SSH transport, channel multiplexing, SFTP.
//!
//! CLAUDE.md §8. The shape that matters:
//!
//!   §8.1  "One SSH transport per host; SSH sessions, SFTP profiles, and port
//!         forwards multiplex as channels over it. This is what makes a new tab
//!         cost 20 ms while Termius re-handshakes for 900 ms."
//!   §8.1  "But: the PRD requires multiple SFTP identities per host, and
//!         different usernames/keys need *separate* transports."
//!   §14.6 "TCP_NODELAY on every socket." russh defaults this OFF.
//!   §6.1  "Widen the SSH channel window too. russh defaults are small."

pub mod agent;
pub mod forward;
pub mod hostkey;
pub mod sftp;
pub mod transport;

pub use forward::{Forward, ForwardKind, ForwardStats};
pub use hostkey::{HostKeyPolicy, Verdict};
pub use sftp::{Entry, EntryKind, Sftp};
pub use agent::AgentStatus;
pub use transport::{AuthMethod, Endpoint, Negotiated, Shell, SshError, Timeouts, Transport};
