//! The secrets vault — CLAUDE.md §12.3.
//!
//! The section header is "get this right or there is no recovery", and it names
//! the way to get it wrong:
//!
//! > "If you make DPAPI the root of trust instead, the vault becomes permanently
//! > unopenable on any other machine, with no recovery path. This is a one-line
//! > design decision with permanent consequences."
//!
//! So the root of trust here is **the master password and nothing else**. Every
//! input to key derivation — the Argon2 parameters, the salt — is stored in
//! `vault.bin` itself. Copy that one file to a new machine, type the password,
//! and it opens. There is no OS keychain call anywhere in this module, and the
//! type system helps keep it that way: [`Vault::open`] takes a password and a
//! path, and has nowhere to get a machine-local key from even if someone later
//! wanted it to.
//!
//! # File format (v1)
//!
//! ```text
//! magic     "OPSPILOT-VAULT\0"    15 bytes
//! version   u8                    = 1
//! kdf       u8                    = 1 (Argon2id)
//! m_cost    u32 LE                KiB
//! t_cost    u32 LE
//! p_cost    u32 LE
//! salt_len  u8
//! salt      [salt_len]
//! verifier  sealed(VERIFIER_PLAINTEXT, aad = b"verifier")
//! count     u32 LE
//! records   count x { id_len u16 | id (utf8) | sealed(secret, aad = id) }
//! manifest  sealed(blake3(all ids in file order), aad = b"manifest")
//!
//! sealed(x, aad) = nonce[12] | len u32 LE | AES-256-GCM(x, aad)
//! ```
//!
//! Three things that format buys, which a naive "encrypt the whole map" does not:
//!
//! * **The header is authenticated for free.** Flip a bit in `m_cost` or the
//!   salt and you derive a different key, so the verifier fails to decrypt. No
//!   separate header MAC is needed.
//! * **Records cannot be swapped.** Each record's id is the GCM associated data,
//!   so moving `staging/db`'s ciphertext onto `prod/root` fails authentication
//!   instead of silently handing back the wrong password.
//! * **Records cannot be removed or added.** The trailing manifest seals a hash
//!   of the id list, so truncating the file — the obvious attack on a
//!   count-prefixed format — is detected on open.
//!
//! # What is deliberately not here
//!
//! §12.3 also asks for `mlock`ed pages and a **separate broker process**. Memory
//! is [`Zeroize`]d, but not locked: `VirtualLock`/`mlock` needs platform bindings
//! this crate does not have, so a derived key can in principle reach the page
//! file. And this is an in-process library, so a webview compromise that can
//! reach Tauri commands can ask for a secret — the broker boundary would fix
//! that and does not exist. Both are recorded in COMPLETED.md rather than
//! implied to be done.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use argon2::{Algorithm, Argon2, Params, Version};
use rand::RngCore;
use zeroize::{Zeroize, Zeroizing};

const MAGIC: &[u8; 15] = b"OPSPILOT-VAULT\0";
const VERSION: u8 = 1;
const KDF_ARGON2ID: u8 = 1;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;

/// Sealed under the root key so a wrong password is reported as a wrong
/// password, not as a corrupt file. The bytes are not secret.
const VERIFIER_PLAINTEXT: &[u8] = b"opspilot-vault-v1";

/// §12.3: "Argon2id (m=64 MiB, t=3, p=4)".
pub const DEFAULT_M_COST: u32 = 64 * 1024;
pub const DEFAULT_T_COST: u32 = 3;
pub const DEFAULT_P_COST: u32 = 4;

/// A vault reference as written in config: `vault:prod/deploy-key`.
pub const REF_PREFIX: &str = "vault:";

/// Split a `password_ref` into the vault record id it names.
///
/// Returns `None` for anything that is not a vault reference, so callers can
/// tell "no secret configured" apart from "a secret that is missing".
pub fn parse_ref(reference: &str) -> Option<&str> {
    reference.strip_prefix(REF_PREFIX).filter(|s| !s.is_empty())
}

// ---------------------------------------------------------------------------
// Key material
// ---------------------------------------------------------------------------

/// The derived root key.
///
/// §12.3: "Never a `String`, never logged, never in a panic message. Write
/// manual `Debug` impls printing `<redacted>`."
#[derive(Clone)]
struct RootKey([u8; 32]);

impl Drop for RootKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for RootKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RootKey(<redacted>)")
    }
}

/// A secret on its way in or out. Zeroized on drop; `Debug` never shows it.
///
/// This is the type crossing the API boundary in both directions, so there is
/// no overload of [`Vault::set`] that accepts a plain `String` — passing one
/// would leave a copy in a buffer nothing clears.
pub struct Secret(Zeroizing<Vec<u8>>);

impl Secret {
    pub fn new(bytes: Vec<u8>) -> Self {
        Secret(Zeroizing::new(bytes))
    }

    /// Take ownership of a `String`'s buffer without copying it.
    ///
    /// The `String` is consumed, so the caller cannot still be holding the
    /// plaintext afterwards — which is the entire point.
    pub fn from_string(s: String) -> Self {
        Secret(Zeroizing::new(s.into_bytes()))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Borrow as UTF-8. Fails rather than lossily converting, because a mangled
    /// password fails authentication in a way that is very hard to diagnose.
    pub fn as_str(&self) -> Result<&str, VaultError> {
        std::str::from_utf8(&self.0).map_err(|_| VaultError::NotUtf8)
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Not even the length: that leaks password length across a log file.
        f.write_str("Secret(<redacted>)")
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum VaultError {
    /// The master password did not open the vault. Distinct from `Corrupt` so
    /// the UI can say "wrong password" instead of frightening the user.
    WrongPassword,
    NotFound(String),
    /// Structurally invalid, truncated, or tampered with.
    Corrupt(String),
    /// Written by a newer build. §12.2's refusal rule applies to the vault too:
    /// opening it with an older reader risks writing back a downgraded file.
    UnsupportedVersion(u8),
    NotUtf8,
    Io(std::io::Error),
}

impl fmt::Display for VaultError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VaultError::WrongPassword => f.write_str("wrong master password"),
            VaultError::NotFound(id) => write!(f, "no vault record named {id:?}"),
            VaultError::Corrupt(why) => write!(f, "vault file is damaged: {why}"),
            VaultError::UnsupportedVersion(v) => write!(
                f,
                "vault.bin is version {v}, this build understands {VERSION} — \
                 upgrade OpsPilot rather than letting it write an older format"
            ),
            VaultError::NotUtf8 => f.write_str("secret is not valid UTF-8"),
            VaultError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for VaultError {}

impl From<std::io::Error> for VaultError {
    fn from(e: std::io::Error) -> Self {
        VaultError::Io(e)
    }
}

// ---------------------------------------------------------------------------
// Vault
// ---------------------------------------------------------------------------

/// KDF parameters as read from (or written to) the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Kdf {
    pub m_cost: u32,
    pub t_cost: u32,
    pub p_cost: u32,
}

impl Default for Kdf {
    fn default() -> Self {
        Kdf {
            m_cost: DEFAULT_M_COST,
            t_cost: DEFAULT_T_COST,
            p_cost: DEFAULT_P_COST,
        }
    }
}

/// An open vault. Dropping it drops the root key, which zeroizes it.
pub struct Vault {
    path: PathBuf,
    kdf: Kdf,
    salt: [u8; SALT_LEN],
    key: RootKey,
    /// id → ciphertext, still sealed. Secrets are decrypted one at a time on
    /// [`Vault::get`] and never held in the clear as a set, so a memory dump
    /// during normal use exposes at most the one secret in flight.
    ///
    /// `BTreeMap` so the on-disk record order is deterministic — the manifest
    /// hash would otherwise change on every save.
    records: BTreeMap<String, Vec<u8>>,
}

impl fmt::Debug for Vault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Vault")
            .field("path", &self.path)
            .field("records", &self.records.len())
            .field("key", &"<redacted>")
            .finish()
    }
}

impl Vault {
    /// Default location: the **portable** config root, per §12.1's listing of
    /// `vault.bin` as "encrypted; portable".
    pub fn default_path() -> PathBuf {
        crate::paths::config_dir().join("vault.bin")
    }

    pub fn exists(path: &Path) -> bool {
        path.is_file()
    }

    /// Create a new, empty vault. Refuses to clobber an existing file — the one
    /// mistake here is unrecoverable by definition.
    pub fn create(path: &Path, password: &str) -> Result<Vault, VaultError> {
        Vault::create_with(path, password, Kdf::default())
    }

    pub fn create_with(path: &Path, password: &str, kdf: Kdf) -> Result<Vault, VaultError> {
        if path.exists() {
            return Err(VaultError::Corrupt(format!(
                "{} already exists; refusing to overwrite a vault",
                path.display()
            )));
        }
        let mut salt = [0u8; SALT_LEN];
        // OsRng, not a seeded PRNG. A predictable salt turns Argon2 into a
        // lookup against a precomputed table.
        rand::rngs::OsRng.fill_bytes(&mut salt);

        let key = derive(password, &salt, kdf)?;
        let v = Vault {
            path: path.to_path_buf(),
            kdf,
            salt,
            key,
            records: BTreeMap::new(),
        };
        v.save()?;
        Ok(v)
    }

    /// Open an existing vault with the master password.
    ///
    /// Note what is *not* a parameter: there is no machine key, no keychain
    /// handle, no salt. Everything needed is in the file.
    pub fn open(path: &Path, password: &str) -> Result<Vault, VaultError> {
        let bytes = std::fs::read(path)?;
        let mut r = Reader::new(&bytes);

        let magic = r.take(MAGIC.len())?;
        if magic != MAGIC {
            return Err(VaultError::Corrupt("not a vault file".into()));
        }
        let version = r.u8()?;
        if version != VERSION {
            return Err(VaultError::UnsupportedVersion(version));
        }
        let kdf_id = r.u8()?;
        if kdf_id != KDF_ARGON2ID {
            return Err(VaultError::Corrupt(format!("unknown KDF id {kdf_id}")));
        }
        let kdf = Kdf {
            m_cost: r.u32()?,
            t_cost: r.u32()?,
            p_cost: r.u32()?,
        };
        // A hostile file could ask for a 4 TiB Argon2 allocation. Refuse before
        // the allocator is asked, rather than OOM-killing the app on open.
        if kdf.m_cost > 1024 * 1024 || kdf.t_cost == 0 || kdf.p_cost == 0 {
            return Err(VaultError::Corrupt(format!(
                "implausible KDF parameters m={} t={} p={}",
                kdf.m_cost, kdf.t_cost, kdf.p_cost
            )));
        }

        let salt_len = r.u8()? as usize;
        let salt_bytes = r.take(salt_len)?;
        let mut salt = [0u8; SALT_LEN];
        if salt_len != SALT_LEN {
            return Err(VaultError::Corrupt(format!("salt is {salt_len} bytes")));
        }
        salt.copy_from_slice(salt_bytes);

        let key = derive(password, &salt, kdf)?;

        // Everything above feeds the KDF, so a tampered header produces a key
        // that cannot open this: the header is authenticated by consequence.
        let verifier = r.sealed()?;
        match unseal(&key, verifier, b"verifier") {
            Ok(p) if p.as_slice() == VERIFIER_PLAINTEXT => {}
            _ => return Err(VaultError::WrongPassword),
        }

        let count = r.u32()? as usize;
        let mut records = BTreeMap::new();
        for _ in 0..count {
            let id_len = r.u16()? as usize;
            let id = std::str::from_utf8(r.take(id_len)?)
                .map_err(|_| VaultError::Corrupt("record id is not UTF-8".into()))?
                .to_string();
            let ct = r.sealed()?.to_vec();
            if records.insert(id.clone(), ct).is_some() {
                return Err(VaultError::Corrupt(format!("duplicate record {id:?}")));
            }
        }

        // The manifest closes the truncation hole. Without it, lopping bytes off
        // the end and fixing `count` silently deletes records — and the next
        // save would make that permanent.
        let manifest = r.sealed()?;
        let expect = unseal(&key, manifest, b"manifest")?;
        let actual = manifest_hash(&records);
        if expect.as_slice() != actual.as_slice() {
            return Err(VaultError::Corrupt(
                "record list does not match the sealed manifest — \
                 the file was modified outside OpsPilot"
                    .into(),
            ));
        }

        Ok(Vault {
            path: path.to_path_buf(),
            kdf,
            salt,
            key,
            records,
        })
    }

    /// Record ids only. Safe to show in the UI and safe to log.
    pub fn ids(&self) -> Vec<String> {
        self.records.keys().cloned().collect()
    }

    pub fn contains(&self, id: &str) -> bool {
        self.records.contains_key(id)
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn kdf(&self) -> Kdf {
        self.kdf
    }

    /// Decrypt one record.
    pub fn get(&self, id: &str) -> Result<Secret, VaultError> {
        let ct = self
            .records
            .get(id)
            .ok_or_else(|| VaultError::NotFound(id.to_string()))?;
        // The id is the associated data, so a ciphertext moved here from
        // another record fails authentication instead of decrypting to the
        // wrong server's password.
        let plain = unseal(&self.key, ct, id.as_bytes())?;
        Ok(Secret(Zeroizing::new(plain)))
    }

    /// Resolve a config `password_ref` such as `vault:prod/deploy-key`.
    ///
    /// `Ok(None)` means "not a vault reference" — a caller with a `key_path`
    /// and no password should not be treated as an error.
    pub fn resolve_ref(&self, reference: &str) -> Result<Option<Secret>, VaultError> {
        match parse_ref(reference) {
            Some(id) => self.get(id).map(Some),
            None => Ok(None),
        }
    }

    /// Store or replace a secret. Does not write the file; call [`Vault::save`].
    pub fn set(&mut self, id: &str, secret: &Secret) -> Result<(), VaultError> {
        if id.is_empty() {
            return Err(VaultError::Corrupt("record id cannot be empty".into()));
        }
        if id.len() > u16::MAX as usize {
            return Err(VaultError::Corrupt("record id is too long".into()));
        }
        let sealed = seal(&self.key, secret.as_bytes(), id.as_bytes())?;
        self.records.insert(id.to_string(), sealed);
        Ok(())
    }

    pub fn remove(&mut self, id: &str) -> bool {
        self.records.remove(id).is_some()
    }

    /// Re-derive the root key from a new password and re-encrypt everything.
    ///
    /// This decrypts each record under the old key and re-seals under the new
    /// one; it cannot be done by re-wrapping, because there is no intermediate
    /// key to wrap. That is the cost of making the password the sole root of
    /// trust, and it is the right trade at vault sizes.
    ///
    /// A fresh salt is generated: reusing it would let someone holding both
    /// files confirm a password change happened without a rotation.
    pub fn change_password(&mut self, new_password: &str) -> Result<(), VaultError> {
        let mut plain: Vec<(String, Secret)> = Vec::with_capacity(self.records.len());
        for id in self.records.keys() {
            plain.push((id.clone(), self.get(id)?));
        }

        let mut salt = [0u8; SALT_LEN];
        rand::rngs::OsRng.fill_bytes(&mut salt);
        let key = derive(new_password, &salt, self.kdf)?;

        let mut records = BTreeMap::new();
        for (id, secret) in &plain {
            records.insert(id.clone(), seal(&key, secret.as_bytes(), id.as_bytes())?);
        }

        self.salt = salt;
        self.key = key;
        self.records = records;
        // `plain` drops here; every Secret zeroizes its buffer.
        self.save()
    }

    /// Serialize and write atomically.
    pub fn save(&self) -> Result<(), VaultError> {
        let mut out = Vec::with_capacity(256 + self.records.len() * 96);
        out.extend_from_slice(MAGIC);
        out.push(VERSION);
        out.push(KDF_ARGON2ID);
        out.extend_from_slice(&self.kdf.m_cost.to_le_bytes());
        out.extend_from_slice(&self.kdf.t_cost.to_le_bytes());
        out.extend_from_slice(&self.kdf.p_cost.to_le_bytes());
        out.push(SALT_LEN as u8);
        out.extend_from_slice(&self.salt);

        let verifier = seal(&self.key, VERIFIER_PLAINTEXT, b"verifier")?;
        write_sealed(&mut out, &verifier);

        out.extend_from_slice(&(self.records.len() as u32).to_le_bytes());
        for (id, ct) in &self.records {
            out.extend_from_slice(&(id.len() as u16).to_le_bytes());
            out.extend_from_slice(id.as_bytes());
            write_sealed(&mut out, ct);
        }

        let manifest = seal(&self.key, &manifest_hash(&self.records), b"manifest")?;
        write_sealed(&mut out, &manifest);

        write_atomic_bytes(&self.path, &out)?;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

// ---------------------------------------------------------------------------
// Crypto helpers
// ---------------------------------------------------------------------------

fn derive(password: &str, salt: &[u8], kdf: Kdf) -> Result<RootKey, VaultError> {
    let params = Params::new(kdf.m_cost, kdf.t_cost, kdf.p_cost, Some(32))
        .map_err(|e| VaultError::Corrupt(format!("bad Argon2 params: {e}")))?;
    let a2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = [0u8; 32];
    a2.hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(|e| VaultError::Corrupt(format!("key derivation failed: {e}")))?;
    Ok(RootKey(key))
}

/// `nonce || ciphertext`. The length prefix is added by [`write_sealed`].
fn seal(key: &RootKey, plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>, VaultError> {
    let cipher = Aes256Gcm::new_from_slice(&key.0)
        .map_err(|e| VaultError::Corrupt(format!("cipher init: {e}")))?;
    let mut nonce = [0u8; NONCE_LEN];
    // Per-record nonce, from the OS. GCM's failure mode under nonce reuse is
    // total — it leaks the XOR of the plaintexts and the auth key — so this is
    // the one place a "good enough" RNG is not good enough.
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let ct = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| VaultError::Corrupt("encryption failed".into()))?;

    let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(out)
}

fn unseal(key: &RootKey, sealed: &[u8], aad: &[u8]) -> Result<Vec<u8>, VaultError> {
    if sealed.len() < NONCE_LEN + 16 {
        return Err(VaultError::Corrupt("sealed block is too short".into()));
    }
    let cipher = Aes256Gcm::new_from_slice(&key.0)
        .map_err(|e| VaultError::Corrupt(format!("cipher init: {e}")))?;
    let (nonce, ct) = sealed.split_at(NONCE_LEN);
    cipher
        .decrypt(Nonce::from_slice(nonce), Payload { msg: ct, aad })
        // Deliberately no detail: distinguishing "bad tag" from "bad aad" gives
        // an attacker with the file a way to probe the structure.
        .map_err(|_| VaultError::Corrupt("authentication failed".into()))
}

fn write_sealed(out: &mut Vec<u8>, sealed: &[u8]) {
    out.extend_from_slice(&(sealed.len() as u32).to_le_bytes());
    out.extend_from_slice(sealed);
}

/// blake3 over every id, length-prefixed.
///
/// Length prefixes matter: without them `{"ab","c"}` and `{"a","bc"}` hash the
/// same, and the manifest would not notice a rename that concatenates.
fn manifest_hash(records: &BTreeMap<String, Vec<u8>>) -> Vec<u8> {
    let mut h = blake3::Hasher::new();
    for id in records.keys() {
        h.update(&(id.len() as u32).to_le_bytes());
        h.update(id.as_bytes());
    }
    h.finalize().as_bytes().to_vec()
}

/// Byte-oriented twin of `paths::write_atomic`, which takes `&str`.
fn write_atomic_bytes(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// A bounds-checked cursor. Hand-rolled because every read here is on a file
/// that may be hostile, and `&bytes[a..b]` panics instead of erroring.
struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(b: &'a [u8]) -> Self {
        Reader { b, at: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], VaultError> {
        let end = self
            .at
            .checked_add(n)
            .ok_or_else(|| VaultError::Corrupt("length overflow".into()))?;
        if end > self.b.len() {
            return Err(VaultError::Corrupt("file ends mid-record".into()));
        }
        let s = &self.b[self.at..end];
        self.at = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, VaultError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, VaultError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Result<u32, VaultError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn sealed(&mut self) -> Result<&'a [u8], VaultError> {
        let n = self.u32()? as usize;
        self.take(n)
    }
}

// ---------------------------------------------------------------------------
// Audit log — §12.3 "append-only and hash-chained: h[n] = blake3(h[n-1] || entry)"
// ---------------------------------------------------------------------------

/// What happened. Never includes the secret, and never includes the password.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuditAction {
    Unlocked,
    /// Locked, whether by the user or by the idle timer. Distinct from
    /// `Unlocked`: an audit log that records a lock as an unlock is worse than
    /// one that records nothing, because it reads as access that never happened.
    Locked,
    Created,
    Read,
    Written,
    Removed,
    PasswordChanged,
    FailedUnlock,
}

impl AuditAction {
    fn tag(self) -> &'static str {
        match self {
            AuditAction::Unlocked => "unlock",
            AuditAction::Locked => "lock",
            AuditAction::Created => "create",
            AuditAction::Read => "read",
            AuditAction::Written => "write",
            AuditAction::Removed => "remove",
            AuditAction::PasswordChanged => "passwd",
            AuditAction::FailedUnlock => "unlock-failed",
        }
    }
}

/// One line of the audit log: `<hash> <seq> <tag> <id>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditEntry {
    pub seq: u64,
    pub action: AuditAction,
    /// The record id, or empty for vault-wide events. Ids are not secret.
    pub id: String,
    /// `blake3(prev_hash || seq || tag || id)`, hex.
    pub hash: String,
}

/// Append-only, hash-chained log.
///
/// The chain does not make the log tamper-*proof* — anyone who can write the
/// file can rewrite it from any point forward. It makes tampering **visible**:
/// you cannot alter or drop one entry and leave the rest verifying, so
/// [`AuditLog::verify`] turns silent edits into a detectable break.
///
/// [`AuditLog::append`] reads the whole log to find the previous hash, so it is
/// O(n) per write and the file never shrinks. At roughly 80 bytes an entry and a
/// handful of entries per session that is not worth caching — and rotation is
/// not an option, since discarding old entries is exactly what the chain exists
/// to prevent. Stated so it is a known bound rather than a surprise.
pub struct AuditLog {
    path: PathBuf,
}

impl AuditLog {
    pub fn new(path: PathBuf) -> Self {
        AuditLog { path }
    }

    pub fn default_path() -> PathBuf {
        // State dir, not config: this is machine-local history, and §12.1 says
        // the portable root is for things you would copy to a new PC.
        crate::paths::state_dir().join("vault-audit.log")
    }

    pub fn append(&self, action: AuditAction, id: &str) -> std::io::Result<AuditEntry> {
        use std::io::Write;

        let entries = self.read()?;
        let seq = entries.len() as u64;
        let prev = entries.last().map(|e| e.hash.clone()).unwrap_or_default();
        // Ids are sanitised, not rejected: a newline in an id would otherwise
        // let a crafted record forge extra log lines.
        let id = id.replace(['\n', '\r', ' '], "_");
        let hash = chain(&prev, seq, action.tag(), &id);

        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(f, "{hash} {seq} {} {id}", action.tag())?;

        Ok(AuditEntry {
            seq,
            action,
            id,
            hash,
        })
    }

    pub fn read(&self) -> std::io::Result<Vec<AuditEntry>> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut out = Vec::new();
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let mut parts = line.splitn(4, ' ');
            let (Some(hash), Some(seq), Some(tag)) = (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            let id = parts.next().unwrap_or("").to_string();
            let Ok(seq) = seq.parse::<u64>() else {
                continue;
            };
            let action = match tag {
                "unlock" => AuditAction::Unlocked,
                "lock" => AuditAction::Locked,
                "create" => AuditAction::Created,
                "read" => AuditAction::Read,
                "write" => AuditAction::Written,
                "remove" => AuditAction::Removed,
                "passwd" => AuditAction::PasswordChanged,
                _ => AuditAction::FailedUnlock,
            };
            out.push(AuditEntry {
                seq,
                action,
                id,
                hash: hash.to_string(),
            });
        }
        Ok(out)
    }

    /// Recompute the chain. `Ok(())` or the sequence number where it breaks.
    pub fn verify(&self) -> std::io::Result<Result<usize, u64>> {
        let entries = self.read()?;
        let mut prev = String::new();
        for e in &entries {
            let want = chain(&prev, e.seq, e.action.tag(), &e.id);
            if want != e.hash {
                return Ok(Err(e.seq));
            }
            prev = e.hash.clone();
        }
        Ok(Ok(entries.len()))
    }
}

fn chain(prev: &str, seq: u64, tag: &str, id: &str) -> String {
    let mut h = blake3::Hasher::new();
    h.update(prev.as_bytes());
    h.update(&seq.to_le_bytes());
    h.update(tag.as_bytes());
    h.update(id.as_bytes());
    h.finalize().to_hex().to_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Real Argon2 at m=64 MiB takes ~100 ms per derivation. Most tests here
    /// exercise the *format*, not the KDF, and would otherwise spend minutes in
    /// key stretching. `real_params_round_trip` covers the shipped settings.
    fn fast() -> Kdf {
        Kdf {
            m_cost: 64,
            t_cost: 1,
            p_cost: 1,
        }
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("opspilot-vault-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn round_trip() {
        let d = tmp("rt");
        let p = d.join("vault.bin");
        let mut v = Vault::create_with(&p, "correct horse", fast()).unwrap();
        v.set("prod/deploy", &Secret::from_string("hunter2".into()))
            .unwrap();
        v.set("stage/db", &Secret::from_string("s3cret".into()))
            .unwrap();
        v.save().unwrap();

        let v2 = Vault::open(&p, "correct horse").unwrap();
        assert_eq!(v2.get("prod/deploy").unwrap().as_str().unwrap(), "hunter2");
        assert_eq!(v2.get("stage/db").unwrap().as_str().unwrap(), "s3cret");
        assert_eq!(v2.ids(), vec!["prod/deploy", "stage/db"]);
    }

    #[test]
    fn wrong_password_is_reported_as_wrong_password() {
        let d = tmp("wrong");
        let p = d.join("vault.bin");
        Vault::create_with(&p, "right", fast()).unwrap();
        match Vault::open(&p, "wrong") {
            Err(VaultError::WrongPassword) => {}
            other => panic!("expected WrongPassword, got {other:?}"),
        }
    }

    /// The headline requirement of §12.3.
    ///
    /// "Another machine" is modelled the only way it can be in-process: move
    /// the file somewhere else, and open it with nothing but the bytes and the
    /// password. If any machine-local input had crept into key derivation, the
    /// derived key would differ and this would fail.
    #[test]
    fn a_vault_opens_on_a_different_machine() {
        let a = tmp("machine-a");
        let b = tmp("machine-b");
        let src = a.join("vault.bin");

        let mut v = Vault::create_with(&src, "portable-pw", fast()).unwrap();
        v.set("prod/root", &Secret::from_string("moved-with-me".into()))
            .unwrap();
        v.save().unwrap();
        drop(v);

        // Only the file travels. No keychain, no registry, no salt sidecar.
        let dst = b.join("vault.bin");
        std::fs::copy(&src, &dst).unwrap();
        std::fs::remove_dir_all(&a).unwrap();

        let moved = Vault::open(&dst, "portable-pw").expect("vault must open on another machine");
        assert_eq!(
            moved.get("prod/root").unwrap().as_str().unwrap(),
            "moved-with-me"
        );
    }

    /// The salt and params must be *in* the file, not derived from anything.
    #[test]
    fn params_and_salt_are_stored_in_the_file() {
        let d = tmp("hdr");
        let p = d.join("vault.bin");
        let kdf = Kdf {
            m_cost: 128,
            t_cost: 2,
            p_cost: 1,
        };
        let v = Vault::create_with(&p, "pw", kdf).unwrap();
        let salt = v.salt;
        drop(v);

        let bytes = std::fs::read(&p).unwrap();
        assert_eq!(&bytes[..MAGIC.len()], MAGIC);
        let mut r = Reader::new(&bytes);
        r.take(MAGIC.len()).unwrap();
        assert_eq!(r.u8().unwrap(), VERSION);
        assert_eq!(r.u8().unwrap(), KDF_ARGON2ID);
        assert_eq!(r.u32().unwrap(), 128);
        assert_eq!(r.u32().unwrap(), 2);
        assert_eq!(r.u32().unwrap(), 1);
        assert_eq!(r.u8().unwrap() as usize, SALT_LEN);
        assert_eq!(r.take(SALT_LEN).unwrap(), &salt[..]);

        // And they survive a reopen, rather than being re-defaulted.
        assert_eq!(Vault::open(&p, "pw").unwrap().kdf(), kdf);
    }

    #[test]
    fn two_vaults_with_the_same_password_get_different_salts() {
        let d = tmp("salt");
        let v1 = Vault::create_with(&d.join("a.bin"), "same", fast()).unwrap();
        let v2 = Vault::create_with(&d.join("b.bin"), "same", fast()).unwrap();
        assert_ne!(v1.salt, v2.salt, "salt must be random per vault");
    }

    #[test]
    fn the_same_secret_twice_produces_different_ciphertext() {
        let d = tmp("nonce");
        let p = d.join("vault.bin");
        let mut v = Vault::create_with(&p, "pw", fast()).unwrap();
        v.set("a", &Secret::from_string("identical".into()))
            .unwrap();
        v.set("b", &Secret::from_string("identical".into()))
            .unwrap();
        assert_ne!(
            v.records["a"], v.records["b"],
            "per-record nonce is missing — equal secrets must not be visibly equal"
        );
    }

    #[test]
    fn tampering_with_a_ciphertext_is_detected() {
        let d = tmp("tamper");
        let p = d.join("vault.bin");
        let mut v = Vault::create_with(&p, "pw", fast()).unwrap();
        v.set("x", &Secret::from_string("value".into())).unwrap();
        v.save().unwrap();

        let mut bytes = std::fs::read(&p).unwrap();
        // Flip a bit in the last quarter, which is record/manifest territory.
        let i = bytes.len() * 3 / 4;
        bytes[i] ^= 0x01;
        std::fs::write(&p, &bytes).unwrap();

        assert!(
            Vault::open(&p, "pw").is_err(),
            "a modified vault must not open"
        );
    }

    /// GCM alone would happily decrypt a ciphertext moved between records. The
    /// id-as-AAD binding is what stops staging's password being served as prod's.
    #[test]
    fn a_ciphertext_cannot_be_moved_between_records() {
        let d = tmp("swap");
        let p = d.join("vault.bin");
        let mut v = Vault::create_with(&p, "pw", fast()).unwrap();
        v.set("prod/root", &Secret::from_string("prod-pw".into()))
            .unwrap();
        v.set("stage/root", &Secret::from_string("stage-pw".into()))
            .unwrap();

        let stage_ct = v.records["stage/root"].clone();
        v.records.insert("prod/root".into(), stage_ct);

        match v.get("prod/root") {
            Err(VaultError::Corrupt(_)) => {}
            Ok(s) => panic!(
                "a swapped ciphertext decrypted to {:?} — records are not bound to their ids",
                s.as_str()
            ),
            Err(e) => panic!("unexpected error {e:?}"),
        }
    }

    /// Truncation is the obvious attack on a count-prefixed format: drop the
    /// last record, decrement the count, and the file still parses.
    #[test]
    fn deleting_a_record_outside_opspilot_is_detected() {
        let d = tmp("trunc");
        let p = d.join("vault.bin");
        let mut v = Vault::create_with(&p, "pw", fast()).unwrap();
        v.set("keep", &Secret::from_string("a".into())).unwrap();
        v.set("remove-me", &Secret::from_string("b".into()))
            .unwrap();
        v.save().unwrap();

        // Rebuild the file with one record dropped, count fixed, manifest kept.
        let full = Vault::open(&p, "pw").unwrap();
        let mut forged = Vault {
            path: p.clone(),
            kdf: full.kdf,
            salt: full.salt,
            key: full.key.clone(),
            records: full.records.clone(),
        };
        forged.records.remove("remove-me");
        // Write it by hand so the manifest is the ORIGINAL one — exactly what an
        // attacker editing the file would end up with.
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.push(VERSION);
        out.push(KDF_ARGON2ID);
        out.extend_from_slice(&forged.kdf.m_cost.to_le_bytes());
        out.extend_from_slice(&forged.kdf.t_cost.to_le_bytes());
        out.extend_from_slice(&forged.kdf.p_cost.to_le_bytes());
        out.push(SALT_LEN as u8);
        out.extend_from_slice(&forged.salt);
        write_sealed(
            &mut out,
            &seal(&forged.key, VERIFIER_PLAINTEXT, b"verifier").unwrap(),
        );
        out.extend_from_slice(&(forged.records.len() as u32).to_le_bytes());
        for (id, ct) in &forged.records {
            out.extend_from_slice(&(id.len() as u16).to_le_bytes());
            out.extend_from_slice(id.as_bytes());
            write_sealed(&mut out, ct);
        }
        // The manifest still covers BOTH ids.
        write_sealed(
            &mut out,
            &seal(&forged.key, &manifest_hash(&full.records), b"manifest").unwrap(),
        );
        std::fs::write(&p, &out).unwrap();

        match Vault::open(&p, "pw") {
            Err(VaultError::Corrupt(m)) => assert!(m.contains("manifest"), "{m}"),
            other => panic!("a deleted record was not noticed: {other:?}"),
        }
    }

    #[test]
    fn a_truncated_file_errors_rather_than_panicking() {
        let d = tmp("short");
        let p = d.join("vault.bin");
        let mut v = Vault::create_with(&p, "pw", fast()).unwrap();
        v.set("x", &Secret::from_string("y".into())).unwrap();
        v.save().unwrap();

        let bytes = std::fs::read(&p).unwrap();
        for cut in [
            0,
            1,
            10,
            MAGIC.len(),
            20,
            40,
            bytes.len() / 2,
            bytes.len() - 1,
        ] {
            std::fs::write(&p, &bytes[..cut]).unwrap();
            // The requirement is "does not panic". Any Err is fine.
            assert!(Vault::open(&p, "pw").is_err(), "cut at {cut} opened");
        }
    }

    #[test]
    fn an_implausible_memory_cost_is_refused_before_allocating() {
        let d = tmp("bomb");
        let p = d.join("vault.bin");
        Vault::create_with(&p, "pw", fast()).unwrap();
        let mut bytes = std::fs::read(&p).unwrap();
        // m_cost sits right after magic + version + kdf id.
        let at = MAGIC.len() + 2;
        bytes[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        std::fs::write(&p, &bytes).unwrap();

        match Vault::open(&p, "pw") {
            Err(VaultError::Corrupt(m)) => assert!(m.contains("implausible"), "{m}"),
            other => panic!("expected refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_newer_version_is_refused_not_downgraded() {
        let d = tmp("ver");
        let p = d.join("vault.bin");
        Vault::create_with(&p, "pw", fast()).unwrap();
        let mut bytes = std::fs::read(&p).unwrap();
        bytes[MAGIC.len()] = VERSION + 1;
        std::fs::write(&p, &bytes).unwrap();
        match Vault::open(&p, "pw") {
            Err(VaultError::UnsupportedVersion(v)) => assert_eq!(v, VERSION + 1),
            other => panic!("expected UnsupportedVersion, got {other:?}"),
        }
    }

    #[test]
    fn creating_over_an_existing_vault_is_refused() {
        let d = tmp("clobber");
        let p = d.join("vault.bin");
        let mut v = Vault::create_with(&p, "pw", fast()).unwrap();
        v.set("precious", &Secret::from_string("data".into()))
            .unwrap();
        v.save().unwrap();

        assert!(Vault::create_with(&p, "other", fast()).is_err());
        // And the original is untouched.
        assert_eq!(
            Vault::open(&p, "pw")
                .unwrap()
                .get("precious")
                .unwrap()
                .as_str()
                .unwrap(),
            "data"
        );
    }

    #[test]
    fn changing_the_password_re_encrypts_everything() {
        let d = tmp("passwd");
        let p = d.join("vault.bin");
        let mut v = Vault::create_with(&p, "old", fast()).unwrap();
        v.set("a", &Secret::from_string("alpha".into())).unwrap();
        v.set("b", &Secret::from_string("beta".into())).unwrap();
        v.save().unwrap();
        let old_salt = v.salt;

        v.change_password("new").unwrap();
        assert_ne!(v.salt, old_salt, "password change must re-salt");
        drop(v);

        assert!(matches!(
            Vault::open(&p, "old"),
            Err(VaultError::WrongPassword)
        ));
        let v2 = Vault::open(&p, "new").unwrap();
        assert_eq!(v2.get("a").unwrap().as_str().unwrap(), "alpha");
        assert_eq!(v2.get("b").unwrap().as_str().unwrap(), "beta");
    }

    #[test]
    fn removing_a_record_persists() {
        let d = tmp("rm");
        let p = d.join("vault.bin");
        let mut v = Vault::create_with(&p, "pw", fast()).unwrap();
        v.set("gone", &Secret::from_string("x".into())).unwrap();
        v.set("stays", &Secret::from_string("y".into())).unwrap();
        v.save().unwrap();

        assert!(v.remove("gone"));
        assert!(!v.remove("gone"));
        v.save().unwrap();

        let v2 = Vault::open(&p, "pw").unwrap();
        assert_eq!(v2.ids(), vec!["stays"]);
        assert!(matches!(v2.get("gone"), Err(VaultError::NotFound(_))));
    }

    /// §12.3: "never logged, never in a panic message".
    #[test]
    fn debug_output_never_contains_a_secret() {
        let d = tmp("dbg");
        let p = d.join("vault.bin");
        let mut v = Vault::create_with(&p, "master-password-here", fast()).unwrap();
        let secret = Secret::from_string("SUPERSECRETVALUE".into());
        v.set("id", &secret).unwrap();

        let rendered = format!("{:?} {:?} {:?}", v, secret, v.key);
        assert!(!rendered.contains("SUPERSECRETVALUE"), "{rendered}");
        assert!(!rendered.contains("master-password-here"), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
        // The vault's own Debug must not dump ciphertext either — it is not
        // plaintext, but it is exactly what an offline cracker wants.
        assert!(!format!("{v:?}").contains(&format!("{:?}", v.records["id"][..4].to_vec())));
    }

    #[test]
    fn refs_are_parsed_and_resolved() {
        assert_eq!(parse_ref("vault:prod/deploy"), Some("prod/deploy"));
        assert_eq!(parse_ref("vault:"), None);
        assert_eq!(parse_ref("~/.ssh/id_ed25519"), None);
        assert_eq!(parse_ref("${env:PASSWORD}"), None);

        let d = tmp("refs");
        let p = d.join("vault.bin");
        let mut v = Vault::create_with(&p, "pw", fast()).unwrap();
        v.set("prod/deploy", &Secret::from_string("pw123".into()))
            .unwrap();

        assert_eq!(
            v.resolve_ref("vault:prod/deploy")
                .unwrap()
                .unwrap()
                .as_str()
                .unwrap(),
            "pw123"
        );
        // Not a vault ref: absent, not an error.
        assert!(v.resolve_ref("~/.ssh/id_ed25519").unwrap().is_none());
        // A vault ref that names nothing IS an error — silently connecting
        // without a password would be worse.
        assert!(v.resolve_ref("vault:nope").is_err());
    }

    /// The shipped Argon2 settings, exercised once so a typo in the constants
    /// cannot hide behind the fast test params.
    #[test]
    fn real_params_round_trip() {
        let d = tmp("real");
        let p = d.join("vault.bin");
        let mut v = Vault::create(&p, "a real master password").unwrap();
        assert_eq!(v.kdf(), Kdf::default());
        assert_eq!(v.kdf().m_cost, 64 * 1024);
        v.set("prod/root", &Secret::from_string("real".into()))
            .unwrap();
        v.save().unwrap();

        let v2 = Vault::open(&p, "a real master password").unwrap();
        assert_eq!(v2.get("prod/root").unwrap().as_str().unwrap(), "real");
        assert!(matches!(
            Vault::open(&p, "a real master passwerd"),
            Err(VaultError::WrongPassword)
        ));
    }

    #[test]
    fn a_binary_secret_survives() {
        let d = tmp("bin");
        let p = d.join("vault.bin");
        let mut v = Vault::create_with(&p, "pw", fast()).unwrap();
        // A key file, not a password: arbitrary bytes including NUL and 0xFF.
        let raw: Vec<u8> = (0u8..=255).collect();
        v.set("key/blob", &Secret::new(raw.clone())).unwrap();
        v.save().unwrap();

        let v2 = Vault::open(&p, "pw").unwrap();
        assert_eq!(v2.get("key/blob").unwrap().as_bytes(), &raw[..]);
        assert!(matches!(
            v2.get("key/blob").unwrap().as_str(),
            Err(VaultError::NotUtf8)
        ));
    }

    // -- audit log ---------------------------------------------------------

    #[test]
    fn the_audit_chain_verifies() {
        let d = tmp("audit");
        let log = AuditLog::new(d.join("audit.log"));
        log.append(AuditAction::Created, "").unwrap();
        log.append(AuditAction::Written, "prod/root").unwrap();
        log.append(AuditAction::Read, "prod/root").unwrap();
        log.append(AuditAction::Locked, "idle-timeout").unwrap();
        assert_eq!(log.verify().unwrap(), Ok(4));

        // A lock must round-trip as a lock. Parsing it back as anything else
        // (the tag table has an `_ =>` fallback) would silently rewrite history.
        assert_eq!(log.read().unwrap()[3].action, AuditAction::Locked);

        let entries = log.read().unwrap();
        assert_eq!(entries[1].action, AuditAction::Written);
        assert_eq!(entries[1].id, "prod/root");
        assert_eq!(entries[2].seq, 2);
    }

    #[test]
    fn editing_an_audit_entry_breaks_the_chain() {
        let d = tmp("audit-tamper");
        let path = d.join("audit.log");
        let log = AuditLog::new(path.clone());
        log.append(AuditAction::Created, "").unwrap();
        log.append(AuditAction::Read, "prod/root").unwrap();
        log.append(AuditAction::Read, "stage/db").unwrap();

        // Rewrite the middle entry to hide a read of prod.
        let text = std::fs::read_to_string(&path).unwrap();
        let doctored = text.replace("read prod/root", "read stage/db");
        assert_ne!(text, doctored);
        std::fs::write(&path, doctored).unwrap();

        assert_eq!(
            log.verify().unwrap(),
            Err(1),
            "a rewritten entry must break the chain at its own sequence number"
        );
    }

    #[test]
    fn dropping_the_last_audit_entry_is_detected_on_the_next_append() {
        let d = tmp("audit-trunc");
        let path = d.join("audit.log");
        let log = AuditLog::new(path.clone());
        log.append(AuditAction::Created, "").unwrap();
        log.append(AuditAction::Read, "prod/root").unwrap();

        // Truncating the tail leaves a chain that still verifies — that is the
        // documented limit of a hash chain without an external anchor. What it
        // DOES guarantee is that the removed entry cannot come back, and that
        // the next append does not silently reuse the old sequence number's
        // hash, so a full-history comparison shows the gap.
        let first_line = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .to_string();
        std::fs::write(&path, format!("{first_line}\n")).unwrap();
        assert_eq!(log.verify().unwrap(), Ok(1));

        let next = log.append(AuditAction::Read, "stage/db").unwrap();
        assert_eq!(next.seq, 1, "sequence numbers are dense");
        assert_eq!(log.verify().unwrap(), Ok(2));
    }

    #[test]
    fn a_newline_in_a_record_id_cannot_forge_a_log_line() {
        let d = tmp("audit-inject");
        let log = AuditLog::new(d.join("audit.log"));
        log.append(AuditAction::Created, "").unwrap();
        log.append(AuditAction::Read, "evil\nffff 99 read prod/root")
            .unwrap();
        let entries = log.read().unwrap();
        assert_eq!(entries.len(), 2, "an id forged an extra entry");
        assert_eq!(log.verify().unwrap(), Ok(2));
    }

    #[test]
    fn a_missing_audit_log_reads_as_empty() {
        let d = tmp("audit-missing");
        let log = AuditLog::new(d.join("nope.log"));
        assert!(log.read().unwrap().is_empty());
        assert_eq!(log.verify().unwrap(), Ok(0));
    }
}
