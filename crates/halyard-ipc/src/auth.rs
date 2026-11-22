//! Secrets: session tokens, controller grant tokens, and the grants file.
//!
//! Tokens are 256 bits from the OS CSPRNG. Only their SHA-256 is stored. A fast hash is
//! the right choice here: the input is a uniformly random 256-bit value, so there is
//! nothing for a slow password hash (argon2, scrypt) to protect against.

use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Write};
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
#[derive(Debug)]
pub struct GrantStore {
    path: Option<PathBuf>,
    grants: Vec<Grant>,
}

impl GrantStore {
    /// A store that lives only in memory. Grants vanish when the process exits.
    pub fn in_memory() -> Self {
        Self { path: None, grants: Vec::new() }
    }

    /// Opens (or prepares to create) the grants file at `path`.
    ///
    /// The parent directory is created with mode 0700 if missing. An existing file must
    /// be a regular file owned by us with no group or other bits, otherwise it is refused:
    /// a writable grants file would let someone else add a controller.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, Error> {
        let path = path.into();
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty())
            && !parent.exists()
        {
            ensure_private_dir(parent)?;
        }
        let grants = match fs::symlink_metadata(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(Error::io(format!("inspecting {}", path.display()), e)),
            Ok(_) => {
                check_private(&path, false)?;
                let text = fs::read_to_string(&path)
                    .map_err(|e| Error::io(format!("reading {}", path.display()), e))?;
                let file: GrantsFile = serde_json::from_str(&text).map_err(|e| {
                    Error::CorruptGrants { path: path.clone(), message: e.to_string() }
                })?;
                if file.version != GRANTS_FILE_VERSION {
                    return Err(Error::CorruptGrants {
                        path,
                        message: format!("unsupported version {}", file.version),
                    });
                }
                file.grants
            }
        };
        Ok(Self { path: Some(path), grants })
    }

    /// The backing file, if any.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// All grants.
    pub fn list(&self) -> &[Grant] {
        &self.grants
    }

    /// The grant with this id.
    pub fn get(&self, id: &str) -> Option<&Grant> {
        self.grants.iter().find(|g| g.id == id)
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
        self.grants.push(grant.clone());
        if let Err(e) = self.persist() {
            self.grants.pop();
            return Err(e);
        }
        Ok((grant, token))
    }

    /// Finds the grant for `token`, records the use, and returns it.
    ///
    /// Every stored hash is compared, in constant time, so timing does not reveal which
    /// grant matched or how much of a hash matched.
    pub fn authenticate(&mut self, token: &str) -> Option<Grant> {
        let presented = TokenHash::of(token);
        let mut found = None;
        for (i, grant) in self.grants.iter().enumerate() {
            let stored = TokenHash::from_hex(&grant.token_sha256);
            if stored.is_some_and(|h| constant_time_eq(&h.0, &presented.0)) {
                found = Some(i);
            }
        }
        let i = found?;
        self.grants[i].last_used = Some(now_unix());
        // A failed last-used write must not lock the user out; the grant stays valid.
        if let Err(e) = self.persist() {
            eprintln!("halyard-ipc: could not record grant use: {e}");
        }
        Some(self.grants[i].clone())
    }

    /// Removes a grant. Returns whether it existed. Connections using it lose access on
    /// their next request.
    pub fn revoke(&mut self, id: &str) -> Result<bool, Error> {
        let Some(pos) = self.grants.iter().position(|g| g.id == id) else {
            return Ok(false);
        };
        let removed = self.grants.remove(pos);
        if let Err(e) = self.persist() {
            self.grants.insert(pos, removed);
            return Err(e);
        }
        Ok(true)
    }

    /// Writes the file atomically: a new 0600 temp file in the same directory, fsync,
    /// rename over the old one.
    fn persist(&self) -> Result<(), Error> {
        let Some(path) = &self.path else { return Ok(()) };
        let file = GrantsFile { version: GRANTS_FILE_VERSION, grants: self.grants.clone() };
        let json = serde_json::to_vec_pretty(&file)
            .map_err(|e| Error::io("serializing grants", io::Error::other(e)))?;
        let tmp = path.with_extension(format!(
            "tmp.{}.{}",
            std::process::id(),
            hex(&random_bytes::<4>())
        ));
        let write = || -> io::Result<()> {
            let mut f =
                fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
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
