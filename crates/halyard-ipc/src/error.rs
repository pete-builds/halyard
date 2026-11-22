//! The crate's setup error type.

use std::fmt;
use std::io;
use std::path::PathBuf;

/// Errors from setting up the server or the grant store.
#[derive(Debug)]
pub enum Error {
    /// An I/O error, with what was being done.
    Io {
        /// What was being attempted.
        context: String,
        /// The underlying error.
        source: io::Error,
    },
    /// A file or directory exists but is not safe to use: wrong owner, wrong mode, or a
    /// symlink where a real directory or file is required. Halyard refuses rather than
    /// repairing it, because someone else may control it.
    Insecure {
        /// The offending path.
        path: PathBuf,
        /// Why it was refused.
        reason: String,
    },
    /// Another live server already listens on this socket path.
    AddressInUse(PathBuf),
    /// The socket path does not fit in `sockaddr_un.sun_path`.
    PathTooLong(PathBuf),
    /// A name (instance, session id) contains characters that are not allowed.
    InvalidName(String),
    /// The grants file could not be parsed.
    CorruptGrants {
        /// The grants file.
        path: PathBuf,
        /// The parser message.
        message: String,
    },
}

impl Error {
    pub(crate) fn io(context: impl Into<String>, source: io::Error) -> Self {
        Self::Io { context: context.into(), source }
    }

    pub(crate) fn insecure(path: impl Into<PathBuf>, reason: impl Into<String>) -> Self {
        Self::Insecure { path: path.into(), reason: reason.into() }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { context, source } => write!(f, "{context}: {source}"),
            Self::Insecure { path, reason } => {
                write!(f, "refusing to use {}: {reason}", path.display())
            }
            Self::AddressInUse(path) => {
                write!(f, "another Halyard instance is listening on {}", path.display())
            }
            Self::PathTooLong(path) => {
                write!(f, "socket path is too long for a Unix socket: {}", path.display())
            }
            Self::InvalidName(name) => write!(f, "invalid name: {name:?}"),
            Self::CorruptGrants { path, message } => {
                write!(f, "cannot parse grants file {}: {message}", path.display())
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}
