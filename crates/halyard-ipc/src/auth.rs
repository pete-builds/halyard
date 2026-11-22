//! Secrets: session tokens, controller grant tokens, and the grants file.
//!
//! Tokens are 256 bits from the OS CSPRNG. Only their SHA-256 is stored. A fast hash is
//! the right choice here: the input is a uniformly random 256-bit value, so there is
//! nothing for a slow password hash (argon2, scrypt) to protect against.

use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::Error;
use crate::paths::{check_private, ensure_private_dir};
use crate::types::Scope;

/// Prefix of controller grant tokens, so they are recognizable in logs and secret scans.
pub const GRANT_TOKEN_PREFIX: &str = "hlyg_";
/// Prefix of per-session tokens exported to panes as `HALYARD_SESSION_TOKEN`.
pub const SESSION_TOKEN_PREFIX: &str = "hlys_";

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut buf = [0u8; N];
    // The OS CSPRNG does not fail on supported platforms once booted. If it ever does,
    // continuing with a predictable token would be worse than stopping.
    getrandom::fill(&mut buf).expect("OS random number generator failed");
    buf
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 0xf) as usize] as char);
    }
    out
}

fn unhex(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
        let hi = (chunk[0] as char).to_digit(16)?;
        let lo = (chunk[1] as char).to_digit(16)?;
        out[i] = (hi * 16 + lo) as u8;
    }
    Some(out)
}

fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Compares two byte slices in time that depends only on their length.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The SHA-256 of a token. What the server keeps instead of the token.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct TokenHash([u8; 32]);

impl TokenHash {
    /// Hashes a token string.
    pub fn of(token: &str) -> Self {
        Self(Sha256::digest(token.as_bytes()).into())
    }

    /// True when `token` hashes to this value. Constant time in the hash comparison.
    pub fn verify(&self, token: &str) -> bool {
        constant_time_eq(&Self::of(token).0, &self.0)
    }

    /// Lowercase hex, as stored in the grants file.
    pub fn to_hex(&self) -> String {
        hex(&self.0)
    }

    /// Parses 64 hex characters.
    pub fn from_hex(s: &str) -> Option<Self> {
        unhex(s).map(Self)
    }
}

impl std::fmt::Debug for TokenHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TokenHash({}...)", &self.to_hex()[..8])
    }
}

/// A per-session secret the app exports to a pane as `HALYARD_SESSION_TOKEN`.
///
/// It is a second factor next to process ancestry; see `docs/session-api.md` for what it
/// does and does not protect. `Debug` never prints the value.
#[derive(Clone)]
pub struct SessionToken(String);

impl SessionToken {
    /// Generates a fresh 256-bit token.
    pub fn generate() -> Self {
        Self(format!("{SESSION_TOKEN_PREFIX}{}", hex(&random_bytes::<32>())))
    }

    /// The token text, for the pane's environment.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The hash the app keeps in its session table.
    pub fn hash(&self) -> TokenHash {
        TokenHash::of(&self.0)
    }
}

impl std::fmt::Debug for SessionToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionToken(<redacted>)")
    }
}

/// What the server needs to authorize a session-scope connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionAuth {
    /// The pid of the pane's PTY child (usually the shell). Clients must be this process
    /// or a descendant of it.
    pub child_pid: u32,
    /// Hash of the session's [`SessionToken`].
    pub token_hash: TokenHash,
}

/// One approved controller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grant {
    /// Random grant id, safe to show and log.
    pub id: String,
    /// The name the client gave when pairing. Self-reported, not verified.
    pub client_name: String,
    /// The scopes the user approved.
    pub scopes: BTreeSet<Scope>,
    /// Lowercase hex SHA-256 of the token. The token itself is never stored.
    pub token_sha256: String,
    /// Unix seconds when the grant was created.
    pub created: u64,
    /// Unix seconds of the last successful authentication, if any.
    pub last_used: Option<u64>,
    /// The user chose "always allow typing for this client". Even then, text containing
    /// control characters (newline, carriage return, escape, ...) needs a fresh approval.
    pub always_allow_input: bool,
}

impl Grant {
    /// True when the grant includes `scope`.
    pub fn has(&self, scope: Scope) -> bool {
        self.scopes.contains(&scope)
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GrantsFile {
    version: u32,
    grants: Vec<Grant>,
}

const GRANTS_FILE_VERSION: u32 = 1;

/// The controller grants, optionally persisted to a 0600 JSON file.
///
/// A file-backed store keeps no authoritative copy in memory. Every read and every
/// change takes an exclusive `flock` on `<file>.lock`, reloads the file, works on what is
/// on disk, and writes it back before unlocking. So several stores on one file (two
/// Halyard instances, or the app and a settings tool) always see each other's changes:
/// a revocation in one is honored by the others on their next request, and a `last_used`
/// write from another store can never bring a revoked grant back.
#[derive(Debug)]
pub struct GrantStore {
    path: Option<PathBuf>,
    /// The grants of an in-memory store. Unused when `path` is set.
    memory: Vec<Grant>,
}

/// Holds the exclusive lock on `<grants file>.lock` until dropped.
struct FileLock(fs::File);

impl FileLock {
    fn acquire(path: &Path) -> Result<Self, Error> {
        let lock_path = lock_path(path);
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&lock_path)
            .map_err(|e| Error::io(format!("opening {}", lock_path.display()), e))?;
        loop {
            // SAFETY: flock on a descriptor we own; no memory is passed.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
                return Ok(Self(file));
            }
            let e = io::Error::last_os_error();
            if e.kind() != io::ErrorKind::Interrupted {
                return Err(Error::io(format!("locking {}", lock_path.display()), e));
            }
        }
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        // Closing the descriptor releases the lock too; this just makes it explicit.
        // SAFETY: flock on a descriptor we own.
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

fn lock_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".lock");
    PathBuf::from(name)
}

/// Reads the grants file. A missing file is an empty list.
fn load(path: &Path) -> Result<Vec<Grant>, Error> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(Error::io(format!("inspecting {}", path.display()), e)),
        Ok(_) => check_private(path, false)?,
    }
    let text = fs::read_to_string(path)
        .map_err(|e| Error::io(format!("reading {}", path.display()), e))?;
    let file: GrantsFile = serde_json::from_str(&text)
        .map_err(|e| Error::CorruptGrants { path: path.to_owned(), message: e.to_string() })?;
    if file.version != GRANTS_FILE_VERSION {
        return Err(Error::CorruptGrants {
            path: path.to_owned(),
            message: format!("unsupported version {}", file.version),
        });
    }
    Ok(file.grants)
}

/// Writes the file atomically: a new 0600 temp file in the same directory, fsync, rename
/// over the old one. Callers hold the [`FileLock`].
fn store(path: &Path, grants: &[Grant]) -> Result<(), Error> {
    let file = GrantsFile { version: GRANTS_FILE_VERSION, grants: grants.to_vec() };
    let json = serde_json::to_vec_pretty(&file)
        .map_err(|e| Error::io("serializing grants", io::Error::other(e)))?;
    let tmp =
        path.with_extension(format!("tmp.{}.{}", std::process::id(), hex(&random_bytes::<4>())));
    let write = || -> io::Result<()> {
        let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
        f.write_all(&json)?;
        f.write_all(b"\n")?;
        f.sync_all()?;
        fs::rename(&tmp, path)
    };
    write().map_err(|e| {
        // Best effort: the temp file may not exist if open itself failed.
        let _ = fs::remove_file(&tmp);
        Error::io(format!("writing {}", path.display()), e)
    })
}

/// Index of the grant whose hash matches `token`. Compares every stored hash in constant
/// time, so timing does not reveal which grant matched or how much of a hash matched.
fn find_token(grants: &[Grant], token: &str) -> Option<usize> {
    let presented = TokenHash::of(token);
    let mut found = None;
    for (i, grant) in grants.iter().enumerate() {
        let stored = TokenHash::from_hex(&grant.token_sha256);
        if stored.is_some_and(|h| constant_time_eq(&h.0, &presented.0)) {
            found = Some(i);
        }
    }
    found
}

impl GrantStore {
    /// A store that lives only in memory. Grants vanish when the process exits.
    pub fn in_memory() -> Self {
        Self { path: None, memory: Vec::new() }
    }

    /// Opens the grants file at `path` (created on the first grant).
    ///
    /// The parent directory is created with mode 0700 if missing. An existing file must
    /// be a regular file owned by us with no group or other bits, otherwise it is refused:
    /// a writable grants file would let someone else add a controller. The file is read
    /// here only to fail early; every later operation reads it again under the lock.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, Error> {
        let path = path.into();
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty())
            && !parent.exists()
        {
            ensure_private_dir(parent)?;
        }
        {
            let _lock = FileLock::acquire(&path)?;
            load(&path)?;
        }
        Ok(Self { path: Some(path), memory: Vec::new() })
    }

    /// The backing file, if any.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Runs `f` on the current grants. For a file-backed store: under the lock, on a
    /// fresh read of the file, writing the result back when `f` reports a change.
    fn with_grants<R>(&mut self, f: impl FnOnce(&mut Vec<Grant>) -> (R, bool)) -> Result<R, Error> {
        let Some(path) = &self.path else {
            return Ok(f(&mut self.memory).0);
        };
        let _lock = FileLock::acquire(path)?;
        let mut grants = load(path)?;
        let (result, changed) = f(&mut grants);
        if changed {
            store(path, &grants)?;
        }
        Ok(result)
    }

    /// A fresh snapshot of all grants. Fails closed: an unreadable file lists nothing.
    pub fn list(&mut self) -> Vec<Grant> {
        self.with_grants(|g| (g.clone(), false)).unwrap_or_else(|e| {
            eprintln!("halyard-ipc: {e}");
            Vec::new()
        })
    }

    /// The grant with this id, as it is on disk now. Fails closed: `None` if the file
    /// cannot be read.
    pub fn get(&mut self, id: &str) -> Option<Grant> {
        self.with_grants(|g| (g.iter().find(|g| g.id == id).cloned(), false)).unwrap_or_else(|e| {
            eprintln!("halyard-ipc: {e}");
            None
        })
    }

    /// Creates a grant and returns it with its token. The token is returned exactly once
    /// and is not recoverable from the store.
    pub fn create(
        &mut self,
        client_name: &str,
        scopes: BTreeSet<Scope>,
        always_allow_input: bool,
    ) -> Result<(Grant, String), Error> {
        let token = format!("{GRANT_TOKEN_PREFIX}{}", hex(&random_bytes::<32>()));
        let grant = Grant {
            id: hex(&random_bytes::<8>()),
            client_name: client_name.to_owned(),
            always_allow_input: always_allow_input && scopes.contains(&Scope::InputWrite),
            scopes,
            token_sha256: TokenHash::of(&token).to_hex(),
            created: now_unix(),
            last_used: None,
        };
        let stored = grant.clone();
        self.with_grants(move |g| {
            g.push(stored);
            ((), true)
        })?;
        Ok((grant, token))
    }

    /// Finds the grant for `token`, records the use, and returns it.
    ///
    /// The lookup and the `last_used` write happen in one locked read-modify-write, so the
    /// write only ever touches grants that are on disk at that moment. Fails closed: an
    /// unreadable file authenticates nobody.
    pub fn authenticate(&mut self, token: &str) -> Option<Grant> {
        let result = self.with_grants(|g| match find_token(g, token) {
            Some(i) => {
                g[i].last_used = Some(now_unix());
                (Some(g[i].clone()), true)
            }
            None => (None, false),
        });
        match result {
            Ok(found) => found,
            Err(e) => {
                eprintln!("halyard-ipc: {e}");
                None
            }
        }
    }

    /// Removes a grant. Returns whether it existed. Every store on the same file, and
    /// every connection using the grant, loses access on its next request.
    pub fn revoke(&mut self, id: &str) -> Result<bool, Error> {
        self.with_grants(|g| {
            let before = g.len();
            g.retain(|grant| grant.id != id);
            let removed = g.len() != before;
            (removed, removed)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn scopes(list: &[Scope]) -> BTreeSet<Scope> {
        list.iter().copied().collect()
    }

    #[test]
    fn grants_file_never_contains_the_raw_token() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("state/grants.json");
        let mut store = GrantStore::open(&path).expect("open");
        let (grant, token) =
            store.create("dash", scopes(&[Scope::SessionsRead]), false).expect("create");
        let text = fs::read_to_string(&path).expect("read grants");
        assert!(token.starts_with(GRANT_TOKEN_PREFIX));
        assert!(!text.contains(&token), "raw token leaked into the grants file");
        assert!(!text.contains(&token[GRANT_TOKEN_PREFIX.len()..]), "token body leaked");
        assert!(text.contains(&grant.token_sha256));
        assert_eq!(grant.token_sha256, TokenHash::of(&token).to_hex());
        let mode = fs::metadata(&path).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn grants_survive_reopen_and_revocation_persists() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("grants.json");
        let mut store = GrantStore::open(&path).expect("open");
        let (grant, token) =
            store.create("dash", scopes(&[Scope::SessionsRead]), false).expect("create");
        drop(store);

        let mut store = GrantStore::open(&path).expect("reopen");
        let found = store.authenticate(&token).expect("token still valid after reopen");
        assert_eq!(found.id, grant.id);
        assert!(found.last_used.is_some());
        assert!(store.authenticate("hlyg_wrong").is_none());
        assert!(store.revoke(&grant.id).expect("revoke"));
        assert!(!store.revoke(&grant.id).expect("second revoke is a no-op"));
        drop(store);

        let mut store = GrantStore::open(&path).expect("reopen after revoke");
        assert!(store.authenticate(&token).is_none(), "revoked token must not authenticate");
    }

    #[test]
    fn revocation_in_one_store_holds_in_another_and_after_restart() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("grants.json");
        // Two instances of Halyard on one grants file.
        let mut a = GrantStore::open(&path).expect("open a");
        let mut b = GrantStore::open(&path).expect("open b");
        let (grant, token) =
            a.create("dash", scopes(&[Scope::SessionsRead]), false).expect("create");
        assert!(b.authenticate(&token).is_some(), "B sees A's new grant");

        assert!(a.revoke(&grant.id).expect("revoke in A"));
        assert!(b.get(&grant.id).is_none(), "B sees the revocation at once");
        assert!(b.authenticate(&token).is_none(), "B rejects the revoked token");
        assert!(b.list().is_empty());

        // B creating its own grant (a full write) must not bring A's revoked grant back.
        b.create("other", scopes(&[Scope::UiFocus]), false).expect("create in B");
        drop((a, b));
        let mut restarted = GrantStore::open(&path).expect("reopen");
        assert!(restarted.authenticate(&token).is_none(), "still revoked after restart");
        assert_eq!(restarted.list().len(), 1);
    }

    #[test]
    fn last_used_write_never_resurrects_a_revoked_grant() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("grants.json");
        let mut a = GrantStore::open(&path).expect("open a");
        let (grant, token) =
            a.create("dash", scopes(&[Scope::SessionsRead]), false).expect("create");
        let (_, other_token) =
            a.create("keep", scopes(&[Scope::SessionsRead]), false).expect("create");
        let mut b = GrantStore::open(&path).expect("open b");
        assert!(b.authenticate(&token).is_some());
        a.revoke(&grant.id).expect("revoke");
        // B records a use of a different grant, which rewrites the file.
        assert!(b.authenticate(&other_token).is_some());
        let text = fs::read_to_string(&path).expect("read");
        assert!(!text.contains(&grant.id), "revoked grant was written back: {text}");
        assert!(GrantStore::open(&path).expect("reopen").authenticate(&token).is_none());
    }

    #[test]
    fn refuses_group_readable_grants_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("grants.json");
        fs::write(&path, br#"{"version":1,"grants":[]}"#).expect("write");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("chmod");
        assert!(matches!(GrantStore::open(&path), Err(Error::Insecure { .. })));
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("chmod");
        assert!(GrantStore::open(&path).is_ok());
    }

    #[test]
    fn always_allow_input_requires_the_input_scope() {
        let mut store = GrantStore::in_memory();
        let (grant, _) = store.create("x", scopes(&[Scope::SessionsRead]), true).expect("create");
        assert!(!grant.always_allow_input);
    }

    #[test]
    fn session_token_verifies_and_hides_in_debug() {
        let token = SessionToken::generate();
        assert!(token.as_str().starts_with(SESSION_TOKEN_PREFIX));
        assert_eq!(token.as_str().len(), SESSION_TOKEN_PREFIX.len() + 64);
        assert!(token.hash().verify(token.as_str()));
        assert!(!token.hash().verify("hlys_other"));
        assert!(!format!("{token:?}").contains(&token.as_str()[5..]));
        assert_ne!(SessionToken::generate().as_str(), token.as_str());
    }

    #[test]
    fn hex_round_trip() {
        let h = TokenHash::of("abc");
        assert_eq!(TokenHash::from_hex(&h.to_hex()), Some(h));
        assert_eq!(TokenHash::from_hex("zz"), None);
    }
}
