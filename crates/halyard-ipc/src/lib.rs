//! Halyard's local session API.
//!
//! A Unix domain socket speaking newline-delimited JSON-RPC 2.0, with two kinds of
//! clients:
//!
//! - **Session scope.** Agents and hooks running inside a pane report state and post
//!   notifications for that pane only. Authority comes from process ancestry (the peer
//!   process must descend from the pane's PTY child, checked with kernel-reported peer
//!   credentials) plus a per-session token from the pane's environment.
//! - **Controller scope.** External tools (dashboards, editors, scripts) pair once with
//!   the user's approval and get a token with named scopes. Typing into a pane always
//!   goes through the user unless they chose "always allow" for that client, and even
//!   then text with a newline or any other control character asks every time.
//!
//! The server is a library: the app implements [`SessionDirectory`] and [`Approver`],
//! calls [`IpcServer::bind`], and publishes events through [`EventHub`]. The
//! `halyard-msg` binary is a client for hooks.
//!
//! The full protocol, error codes and threat model are in `docs/session-api.md`.

pub mod auth;
pub mod client;
pub mod error;
pub mod events;
pub mod osc;
pub mod paths;
pub mod peer;
pub mod rpc;
pub mod server;
pub mod text;
pub mod types;

pub use auth::{Grant, GrantStore, SessionAuth, SessionToken, TokenHash};
pub use client::{Client, ClientError};
pub use error::Error;
pub use events::EventHub;
pub use rpc::{RpcError, code};
pub use server::{
    Approver, DirectoryError, GrantsHandle, IpcServer, Limits, PairingDecision, PairingRequest,
    SendTextReason, SendTextRequest, ServerConfig, SessionDirectory,
};
pub use types::{AgentState, PeerInfo, Scope, SessionEvent, SessionInfo, StateReport};

/// The protocol version this crate speaks.
pub const PROTOCOL_VERSION: u32 = 1;

/// Largest request line the server accepts, in bytes, excluding the newline (64 KiB).
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;

/// Environment variable holding the socket path, set in every pane.
pub const ENV_SOCKET: &str = "HALYARD_SOCKET";
/// Environment variable holding the pane's session id, set in every pane.
pub const ENV_SESSION_ID: &str = "HALYARD_SESSION_ID";
/// Environment variable holding the pane's session token, set in every pane.
pub const ENV_SESSION_TOKEN: &str = "HALYARD_SESSION_TOKEN";

/// The environment variables the app sets for a pane's child process.
pub fn session_env(
    socket: &std::path::Path,
    session_id: &str,
    token: &SessionToken,
) -> Vec<(&'static str, std::ffi::OsString)> {
    vec![
        (ENV_SOCKET, socket.as_os_str().to_owned()),
        (ENV_SESSION_ID, session_id.into()),
        (ENV_SESSION_TOKEN, token.as_str().into()),
    ]
}
