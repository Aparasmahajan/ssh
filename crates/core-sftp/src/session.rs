//! One SFTP channel, driven at the request level.
//!
//! `russh-sftp`'s `File` keeps one request in flight: read, wait a round trip,
//! read again. That is WinSCP's design and the reason it caps at
//! chunk / RTT (§2). `RawSftpSession` takes `&self` and matches responses by
//! request id, so any number of requests can be outstanding on one channel.
//! The engine uses it directly.

use std::sync::Arc;

use opspilot_core_ssh::russh_sftp::client::rawsession::Limits;
use opspilot_core_ssh::russh_sftp::client::RawSftpSession;
use opspilot_core_ssh::Transport;

use crate::Error;

/// Without `limits@openssh.com`, 32 KiB is the size every SFTP server must
/// accept (draft-ietf-secsh-filexfer-02 §3).
const DEFAULT_IO_LEN: u32 = 32 * 1024;
/// §6.1's chunk size. OpenSSH reports 255 KiB, so its limit wins there.
const MAX_IO_LEN: u32 = 256 * 1024;

/// Responses can be slow behind a deep window on a slow link; the crate's
/// 10 s default would fail healthy transfers. Stalls are handled by the
/// window, not by this.
const REQUEST_TIMEOUT_S: u64 = 120;

pub struct Session {
    pub raw: Arc<RawSftpSession>,
    pub read_len: u32,
    pub write_len: u32,
    /// `posix-rename@openssh.com`: rename over an existing file atomically.
    pub posix_rename: bool,
}

impl Session {
    pub async fn open(transport: &Transport) -> Result<Session, Error> {
        let channel = transport.open_sftp_channel().await.map_err(Error::ssh)?;
        let mut raw = RawSftpSession::new(channel.into_stream());
        let version = raw.init().await.map_err(Error::sftp)?;
        let has = |ext: &str| version.extensions.contains_key(ext);

        let mut read_len = DEFAULT_IO_LEN;
        let mut write_len = DEFAULT_IO_LEN;
        if has("limits@openssh.com") {
            if let Ok(l) = raw.limits().await {
                // Leave a little under the packet limit for headers.
                if l.max_read_len > 0 {
                    read_len = l.max_read_len.min(MAX_IO_LEN as u64) as u32;
                }
                if l.max_write_len > 0 {
                    write_len = l.max_write_len.min(MAX_IO_LEN as u64) as u32;
                }
                raw.set_limits(Limits::from(l));
            }
        }
        raw.set_timeout(REQUEST_TIMEOUT_S);
        Ok(Session {
            raw: Arc::new(raw),
            read_len,
            write_len,
            posix_rename: has("posix-rename@openssh.com"),
        })
    }

    /// `rename(from, to)`, replacing `to` if `replace`. With
    /// `posix-rename@openssh.com` the replace is atomic; without it, `to` is
    /// removed first, which leaves a moment with neither.
    pub async fn rename(&self, from: &str, to: &str, replace: bool) -> Result<(), Error> {
        if replace && self.posix_rename {
            let mut data = Vec::with_capacity(8 + from.len() + to.len());
            for s in [from, to] {
                data.extend_from_slice(&(s.len() as u32).to_be_bytes());
                data.extend_from_slice(s.as_bytes());
            }
            use opspilot_core_ssh::russh_sftp::protocol::{Packet, StatusCode};
            return match self
                .raw
                .extended("posix-rename@openssh.com", data)
                .await
                .map_err(Error::sftp)?
            {
                Packet::Status(s) if s.status_code == StatusCode::Ok => Ok(()),
                Packet::Status(s) => Err(Error::Remote(format!("rename {to}: {}", s.error_message))),
                _ => Err(Error::Remote(format!("rename {to}: unexpected reply"))),
            };
        }
        if replace {
            let _ = self.raw.remove(to).await;
        }
        self.raw.rename(from, to).await.map(|_| ()).map_err(Error::sftp)
    }
}
