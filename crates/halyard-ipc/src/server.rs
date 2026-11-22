//! The socket server: accept loop, per-connection threads, authentication and dispatch.
//!
//! Threading: one accept thread plus one thread per connection (and one more per event
//! subscription). Nothing here runs on, or blocks, the UI thread or a terminal's I/O
//! path. Calls into [`SessionDirectory`] happen on connection threads, so the app's
//! implementation must be thread safe and should hand work to its own threads instead of
//! doing it inline. [`Approver`] calls block the requesting connection only.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::{Grant, GrantStore, SessionAuth};
use crate::error::Error;
use crate::events::EventHub;
use crate::paths::{MAX_SOCKET_PATH_BYTES, ensure_private_dir};
use crate::peer::{current_euid, is_descendant, peer_info};
use crate::rpc::{self, RpcError, code};
use crate::text;
use crate::types::{AgentState, PeerInfo, Scope, SessionInfo, StateReport};
use crate::{MAX_MESSAGE_BYTES, PROTOCOL_VERSION};

/// An error the app returns from a [`SessionDirectory`] action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectoryError {
    /// The session does not exist.
    NotFound,
    /// The action could not be performed; the message is shown to the client.
    Unavailable(String),
}

/// The app's view of its sessions, implemented by the app and called from connection
/// threads.
pub trait SessionDirectory: Send + Sync {
    /// Every live session.
    fn list_sessions(&self) -> Vec<SessionInfo>;

    /// One session by id.
    fn session(&self, id: &str) -> Option<SessionInfo> {
        self.list_sessions().into_iter().find(|s| s.id == id)
    }

    /// The PTY child pid and token hash for a session, or `None` if it does not exist.
    fn session_auth(&self, id: &str) -> Option<SessionAuth>;

    /// An agent in the session reported a state. The app decides what to show.
    fn report_state(&self, id: &str, report: &StateReport) -> Result<(), DirectoryError>;

    /// An agent in the session posted a notification. Title and body are untrusted.
    fn notify(&self, id: &str, title: &str, body: &str) -> Result<(), DirectoryError>;

    /// Focus the session's pane (and raise its window).
    fn focus(&self, id: &str) -> Result<(), DirectoryError>;

    /// The visible screen text of the session's pane.
    fn read_screen(&self, id: &str) -> Result<String, DirectoryError>;

    /// Type `text` into the session's pane. Only called after the approval rules in
    /// `docs/session-api.md` passed.
    fn send_text(&self, id: &str, text: &str) -> Result<(), DirectoryError>;
}

/// A controller asking to be paired.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingRequest {
    /// The name the client gave. Self-reported: show it as a claim, not a fact.
    pub client_name: String,
    /// The scopes requested.
    pub scopes: BTreeSet<Scope>,
    /// The connecting process, from the kernel.
    pub peer: PeerInfo,
}

/// The user's answer to a [`PairingRequest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairingDecision {
    /// Refuse the pairing.
    Deny,
    /// Approve these scopes (a subset of the requested ones; extras are ignored).
    Approve {
        /// The approved scopes.
        scopes: BTreeSet<Scope>,
        /// "Always allow typing from this client" without a per-request dialog. Text with
        /// control characters still asks every time.
        always_allow_input: bool,
    },
}

/// Why a `session.send_text` needs the user's approval this time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendTextReason {
    /// The client has no standing "always allow" permission.
    NoStandingPermission,
    /// The text contains control or invisible characters (newline, carriage return,
    /// escape, ...), which always need a fresh approval.
    ControlCharacters,
}

/// A controller asking to type into a pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendTextRequest {
    /// The grant making the request.
    pub grant_id: String,
    /// The grant's self-reported client name.
    pub client_name: String,
    /// The target session.
    pub session: SessionInfo,
    /// The exact text that will be typed.
    pub text: String,
    /// [`text::escape_for_display`] of `text`. Show this, so every control character is
    /// visible.
    pub display_text: String,
    /// Why approval is needed.
    pub reason: SendTextReason,
    /// The connecting process, from the kernel.
    pub peer: PeerInfo,
}

/// Asks the user. The app shows dialogs; tests script the answers.
///
/// Calls block the requesting connection thread until the user answers. Implementations
/// should time out to a denial rather than wait forever. The server shows at most one
/// approval at a time.
pub trait Approver: Send + Sync {
    /// Pair a new controller?
    fn approve_pairing(&self, request: &PairingRequest) -> PairingDecision;
    /// Type this text into this pane?
    fn approve_send_text(&self, request: &SendTextRequest) -> bool;
}

/// Resource limits. The defaults suit the app; tests lower them.
#[derive(Debug, Clone)]
pub struct Limits {
    /// Largest accepted request line, in bytes, excluding the newline.
    pub max_message_bytes: usize,
    /// Token bucket size: requests allowed in a burst.
    pub rate_burst: u32,
    /// Token bucket refill: sustained requests per second.
    pub rate_per_sec: u32,
    /// Rate-limited requests tolerated before the connection is closed.
    pub max_rate_violations: u32,
    /// Concurrent connections; further connections are closed at once.
    pub max_connections: usize,
    /// Time allowed between connecting and a successful `halyard.hello`.
    pub handshake_timeout: Duration,
    /// A write to a client that is not reading fails after this long.
    pub write_timeout: Duration,
    /// Events queued per subscriber before it is disconnected.
    pub event_queue: usize,
    /// `halyard.pair` attempts allowed per connection.
    pub max_pair_attempts: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_message_bytes: MAX_MESSAGE_BYTES,
            rate_burst: 64,
            rate_per_sec: 32,
            max_rate_violations: 64,
            max_connections: 64,
            handshake_timeout: Duration::from_secs(10),
            write_timeout: Duration::from_secs(5),
            event_queue: 256,
            max_pair_attempts: 3,
        }
    }
}

/// Everything [`IpcServer::bind`] needs besides the directory and approver.
#[derive(Debug)]
pub struct ServerConfig {
    /// The socket path. Its parent directory is created 0700 or must already be private.
    pub socket_path: PathBuf,
    /// Controller grants. Use [`GrantStore::open`] for persistence.
    pub grants: GrantStore,
    /// The event hub the app publishes to.
    pub events: EventHub,
    /// Resource limits.
    pub limits: Limits,
}

impl ServerConfig {
    /// A config with default limits and a fresh event hub.
    pub fn new(socket_path: impl Into<PathBuf>, grants: GrantStore) -> Self {
        Self {
            socket_path: socket_path.into(),
            grants,
            events: EventHub::new(),
            limits: Limits::default(),
        }
    }
}

/// A handle to the server's grant store, for the app's "connected tools" settings page.
#[derive(Debug, Clone)]
pub struct GrantsHandle(Arc<Mutex<GrantStore>>);

impl GrantsHandle {
    /// A snapshot of all grants.
    pub fn list(&self) -> Vec<Grant> {
        lock(&self.0).list().to_vec()
    }

    /// Revokes a grant. Connections using it lose access on their next request.
    pub fn revoke(&self, id: &str) -> Result<bool, Error> {
        lock(&self.0).revoke(id)
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

struct Shared {
    directory: Arc<dyn SessionDirectory>,
    approver: Arc<dyn Approver>,
    grants: Arc<Mutex<GrantStore>>,
    events: EventHub,
    limits: Limits,
    euid: u32,
    stop: AtomicBool,
    conns: Mutex<HashMap<u64, UnixStream>>,
    active: AtomicUsize,
    next_conn: AtomicU64,
    /// Serializes approval dialogs so a client cannot stack them up.
    dialog: Mutex<()>,
}

/// The running session API server. Dropping it stops the server and removes the socket.
pub struct IpcServer {
    shared: Arc<Shared>,
    socket_path: PathBuf,
    accept: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for IpcServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IpcServer").field("socket_path", &self.socket_path).finish_non_exhaustive()
    }
}

impl IpcServer {
    /// Creates the socket and starts serving on background threads.
    ///
    /// The parent directory is created with mode 0700, or checked if it exists (refused
    /// when not ours, a symlink, or accessible to group or others). A stale socket from a
    /// crashed instance is replaced; a live one is an error. The socket is chmod 0600.
    pub fn bind(
        config: ServerConfig,
        directory: Arc<dyn SessionDirectory>,
        approver: Arc<dyn Approver>,
    ) -> Result<Self, Error> {
        let path = config.socket_path;
        if path.as_os_str().len() > MAX_SOCKET_PATH_BYTES {
            return Err(Error::PathTooLong(path));
        }
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or_else(|| Error::insecure(&path, "socket path has no parent directory"))?;
        ensure_private_dir(parent)?;
        remove_stale_socket(&path)?;

        let listener = UnixListener::bind(&path)
            .map_err(|e| Error::io(format!("binding {}", path.display()), e))?;
        // The 0700 directory already keeps other users out; this is defense in depth for
        // platforms that honor socket file permissions (Linux).
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .map_err(|e| Error::io(format!("chmod {}", path.display()), e))?;
        listener
            .set_nonblocking(true)
            .map_err(|e| Error::io("setting the listener non-blocking", e))?;

        let shared = Arc::new(Shared {
            directory,
            approver,
            grants: Arc::new(Mutex::new(config.grants)),
            events: config.events,
            limits: config.limits,
            euid: current_euid(),
            stop: AtomicBool::new(false),
            conns: Mutex::new(HashMap::new()),
            active: AtomicUsize::new(0),
            next_conn: AtomicU64::new(1),
            dialog: Mutex::new(()),
        });
        let accept_shared = Arc::clone(&shared);
        let accept = thread::Builder::new()
            .name("halyard-ipc-accept".into())
            .spawn(move || accept_loop(&accept_shared, &listener))
            .map_err(|e| Error::io("spawning the accept thread", e))?;
        Ok(Self { shared, socket_path: path, accept: Some(accept) })
    }

    /// The socket path, for the panes' `HALYARD_SOCKET`.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// The event hub the app publishes session events to.
    pub fn events(&self) -> EventHub {
        self.shared.events.clone()
    }

    /// The grant store, for listing and revoking controllers.
    pub fn grants(&self) -> GrantsHandle {
        GrantsHandle(Arc::clone(&self.shared.grants))
    }

    /// Stops accepting, closes every connection, and removes the socket file.
    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.accept.take()
            && handle.join().is_err()
        {
            eprintln!("halyard-ipc: accept thread panicked");
        }
        for (_, stream) in lock(&self.shared.conns).drain() {
            // Best effort: the peer may already have closed it.
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
        if let Err(e) = fs::remove_file(&self.socket_path)
            && e.kind() != io::ErrorKind::NotFound
        {
            eprintln!("halyard-ipc: removing {}: {e}", self.socket_path.display());
        }
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        if self.accept.is_some() {
            self.stop();
        }
    }
}

fn remove_stale_socket(path: &Path) -> Result<(), Error> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(Error::io(format!("inspecting {}", path.display()), e)),
    };
    if !meta.file_type().is_socket() || meta.uid() != current_euid() {
        return Err(Error::insecure(path, "a non-socket or foreign file is in the way"));
    }
    if UnixStream::connect(path).is_ok() {
        return Err(Error::AddressInUse(path.to_owned()));
    }
    fs::remove_file(path).map_err(|e| Error::io(format!("removing stale {}", path.display()), e))
}

fn accept_loop(shared: &Arc<Shared>, listener: &UnixListener) {
    let fd = listener.as_raw_fd();
    while !shared.stop.load(Ordering::SeqCst) {
        let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
        // SAFETY: `pfd` is one valid pollfd and the count passed is 1.
        let ready = unsafe { libc::poll(&mut pfd, 1, 200) };
        if ready <= 0 {
            continue;
        }
        match listener.accept() {
            Ok((stream, _)) => spawn_connection(shared, stream),
            Err(e)
                if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => {}
            Err(e) => {
                eprintln!("halyard-ipc: accept failed: {e}");
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

fn spawn_connection(shared: &Arc<Shared>, stream: UnixStream) {
    if shared.active.load(Ordering::SeqCst) >= shared.limits.max_connections {
        return; // dropping the stream closes it
    }
    // Accepted sockets inherit O_NONBLOCK from the listener on BSDs (macOS).
    if stream.set_nonblocking(false).is_err() {
        return;
    }
    let id = shared.next_conn.fetch_add(1, Ordering::SeqCst);
    let Ok(registry_copy) = stream.try_clone() else { return };
    lock(&shared.conns).insert(id, registry_copy);
    shared.active.fetch_add(1, Ordering::SeqCst);
    let conn_shared = Arc::clone(shared);
    let spawned = thread::Builder::new().name(format!("halyard-ipc-conn-{id}")).spawn(move || {
        let _guard = ConnGuard { shared: Arc::clone(&conn_shared), id };
        serve_connection(&conn_shared, stream);
    });
    if spawned.is_err() {
        lock(&shared.conns).remove(&id);
        shared.active.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Cleans up a connection even if a directory or approver call panicked.
struct ConnGuard {
    shared: Arc<Shared>,
    id: u64,
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        if let Some(stream) = lock(&self.shared.conns).remove(&self.id) {
            // Best effort: wakes the event forwarder if one is blocked writing.
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
        self.shared.active.fetch_sub(1, Ordering::SeqCst);
    }
}

struct Writer(Mutex<UnixStream>);

impl Writer {
    fn send(&self, value: &Value) -> io::Result<()> {
        let mut line = serde_json::to_vec(value).map_err(io::Error::other)?;
        line.push(b'\n');
        lock(&self.0).write_all(&line)
    }
}

enum ReadOutcome {
    Line(Vec<u8>),
    Eof,
    TooLarge,
}

fn read_line(reader: &mut BufReader<UnixStream>, max: usize) -> io::Result<ReadOutcome> {
    let mut buf = Vec::new();
    let n = reader.by_ref().take(max as u64 + 1).read_until(b'\n', &mut buf)?;
    if n == 0 {
        return Ok(ReadOutcome::Eof);
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
        return Ok(ReadOutcome::Line(buf));
    }
    if buf.len() > max {
        return Ok(ReadOutcome::TooLarge);
    }
    Ok(ReadOutcome::Line(buf)) // final line without a newline before EOF
}

struct Bucket {
    tokens: f64,
    burst: f64,
    per_sec: f64,
    last: Instant,
}

impl Bucket {
    fn new(limits: &Limits) -> Self {
        let burst = f64::from(limits.rate_burst.max(1));
        Self { tokens: burst, burst, per_sec: f64::from(limits.rate_per_sec), last: Instant::now() }
    }

    fn take(&mut self) -> bool {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + elapsed * self.per_sec).min(self.burst);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Identity {
    None,
    Session(String),
    Controller(String),
}

struct Conn {
    shared: Arc<Shared>,
    peer: PeerInfo,
    writer: Arc<Writer>,
    stream: UnixStream,
    initialized: bool,
    identity: Identity,
    pair_attempts: u32,
    subscription: Option<u64>,
}

impl Drop for Conn {
    fn drop(&mut self) {
        if let Some(id) = self.subscription.take() {
            self.shared.events.unsubscribe(id);
        }
    }
}

fn serve_connection(shared: &Arc<Shared>, stream: UnixStream) {
    let Ok(peer) = peer_info(&stream) else { return };
    // Every connection is checked, regardless of socket and directory permissions.
    if peer.uid != shared.euid {
        return;
    }
    let limits = &shared.limits;
    if stream.set_write_timeout(Some(limits.write_timeout)).is_err()
        || stream.set_read_timeout(Some(limits.handshake_timeout)).is_err()
    {
        return;
    }
    let (Ok(write_half), Ok(control)) = (stream.try_clone(), stream.try_clone()) else { return };
    let mut reader = BufReader::new(stream);
    let mut conn = Conn {
        shared: Arc::clone(shared),
        peer,
        writer: Arc::new(Writer(Mutex::new(write_half))),
        stream: control,
        initialized: false,
        identity: Identity::None,
        pair_attempts: 0,
        subscription: None,
    };
    let mut bucket = Bucket::new(limits);
    let mut violations = 0u32;

    loop {
        let line = match read_line(&mut reader, limits.max_message_bytes) {
            Ok(ReadOutcome::Line(line)) => line,
            Ok(ReadOutcome::Eof) | Err(_) => break,
            Ok(ReadOutcome::TooLarge) => {
                let err = RpcError::new(
                    code::MESSAGE_TOO_LARGE,
                    format!("message exceeds {} bytes; closing", limits.max_message_bytes),
                );
                // Best effort: the connection closes either way.
                let _ = conn.writer.send(&rpc::response_err(&Value::Null, &err));
                break;
            }
        };
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let response = if bucket.take() {
            match rpc::parse_request(&line) {
                Ok(req) => match conn.dispatch(&req.method, &req.params) {
                    Ok(result) => rpc::response_ok(&req.id, result),
                    Err(err) => rpc::response_err(&req.id, &err),
                },
                Err((id, err)) => rpc::response_err(&id, &err),
            }
        } else {
            violations += 1;
            let id = rpc::parse_request(&line).map(|r| r.id).unwrap_or_else(|(id, _)| id);
            let err = RpcError::new(code::RATE_LIMITED, "rate limit exceeded").with_data(
                json!({ "rate_per_sec": limits.rate_per_sec, "burst": limits.rate_burst }),
            );
            rpc::response_err(&id, &err)
        };
        if conn.writer.send(&response).is_err() || violations > limits.max_rate_violations {
            break;
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HelloParams {
    protocols: Vec<u32>,
    client_name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AttachParams {
    session_id: String,
    token: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReportStateParams {
    state: AgentState,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    msg: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NotifyParams {
    title: String,
    #[serde(default)]
    body: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PairParams {
    client_name: String,
    scopes: Vec<Scope>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthenticateParams {
    token: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionParams {
    session_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SendTextParams {
    session_id: String,
    text: String,
}

fn invalid(msg: impl Into<String>) -> RpcError {
    RpcError::new(code::INVALID_PARAMS, msg)
}

fn dir_err(e: DirectoryError) -> RpcError {
    match e {
        DirectoryError::NotFound => RpcError::new(code::SESSION_NOT_FOUND, "session not found"),
        DirectoryError::Unavailable(msg) => RpcError::new(code::UNAVAILABLE, msg),
    }
}

/// Every method name, for `METHOD_NOT_FOUND` versus `NOT_INITIALIZED`.
const METHODS: &[&str] = &[
    "halyard.hello",
    "halyard.pair",
    "halyard.authenticate",
    "halyard.unpair",
    "session.attach",
    "session.report_state",
    "session.notify",
    "session.info",
    "sessions.list",
    "session.focus",
    "session.read_screen",
    "session.send_text",
    "events.subscribe",
];

impl Conn {
    fn dispatch(&mut self, method: &str, params: &Value) -> Result<Value, RpcError> {
        if !METHODS.contains(&method) {
            return Err(RpcError::new(code::METHOD_NOT_FOUND, format!("unknown method {method}")));
        }
        if method == "halyard.hello" {
            return self.hello(rpc::params(params)?);
        }
        if !self.initialized {
            return Err(RpcError::new(code::NOT_INITIALIZED, "call halyard.hello first"));
        }
        match method {
            "halyard.pair" => self.pair(rpc::params(params)?),
            "halyard.authenticate" => self.authenticate(rpc::params(params)?),
            "halyard.unpair" => {
                let _: Empty = rpc::params(params)?;
                self.unpair()
            }
            "session.attach" => self.attach(rpc::params(params)?),
            "session.report_state" => self.report_state(rpc::params(params)?),
            "session.notify" => self.notify(rpc::params(params)?),
            "session.info" => {
                let _: Empty = rpc::params(params)?;
                let id = self.own_session()?;
                let info = self.shared.directory.session(&id).ok_or_else(|| {
                    RpcError::new(code::SESSION_NOT_FOUND, "session no longer exists")
                })?;
                Ok(json!({ "session": info }))
            }
            "sessions.list" => {
                let _: Empty = rpc::params(params)?;
                self.grant_with(Scope::SessionsRead)?;
                Ok(json!({ "sessions": self.shared.directory.list_sessions() }))
            }
            "session.focus" => {
                let p: SessionParams = rpc::params(params)?;
                self.grant_with(Scope::UiFocus)?;
                self.shared.directory.focus(&p.session_id).map_err(dir_err)?;
                Ok(json!({}))
            }
            "session.read_screen" => {
                let p: SessionParams = rpc::params(params)?;
                self.grant_with(Scope::ScreenRead)?;
                let text = self.shared.directory.read_screen(&p.session_id).map_err(dir_err)?;
                Ok(json!({ "text": text }))
            }
            "session.send_text" => self.send_text(rpc::params(params)?),
            "events.subscribe" => {
                let _: Empty = rpc::params(params)?;
                self.subscribe()
            }
            _ => Err(RpcError::new(code::INTERNAL_ERROR, "method table out of sync")),
        }
    }

    fn hello(&mut self, p: HelloParams) -> Result<Value, RpcError> {
        if self.initialized {
            return Err(RpcError::new(code::INVALID_REQUEST, "halyard.hello was already called"));
        }
        text::check_display("client_name", &p.client_name, 64).map_err(invalid)?;
        if !p.protocols.contains(&PROTOCOL_VERSION) {
            return Err(RpcError::new(code::UNSUPPORTED_PROTOCOL, "no common protocol version")
                .with_data(json!({ "supported": [PROTOCOL_VERSION] })));
        }
        self.initialized = true;
        // Handshake done: subscribers may legitimately sit idle for hours.
        if self.stream.set_read_timeout(None).is_err() {
            return Err(RpcError::new(code::INTERNAL_ERROR, "could not clear the read timeout"));
        }
        let limits = &self.shared.limits;
        Ok(json!({
            "protocol": PROTOCOL_VERSION,
            "server": "halyard",
            "server_version": env!("CARGO_PKG_VERSION"),
            "limits": {
                "max_message_bytes": limits.max_message_bytes,
                "rate_per_sec": limits.rate_per_sec,
                "rate_burst": limits.rate_burst,
            },
        }))
    }

    fn require_anonymous(&self) -> Result<(), RpcError> {
        if self.identity == Identity::None {
            Ok(())
        } else {
            Err(RpcError::new(code::INVALID_REQUEST, "this connection already has an identity"))
        }
    }

    fn attach(&mut self, p: AttachParams) -> Result<Value, RpcError> {
        self.require_anonymous()?;
        text::check_ident("session_id", &p.session_id, 128).map_err(invalid)?;
        let unauthenticated =
            || RpcError::new(code::UNAUTHENTICATED, "unknown session or bad token");
        let auth = self.shared.directory.session_auth(&p.session_id).ok_or_else(unauthenticated)?;
        if !auth.token_hash.verify(&p.token) {
            return Err(unauthenticated());
        }
        if !is_descendant(self.peer.pid, auth.child_pid) {
            return Err(RpcError::new(
                code::NOT_IN_SESSION,
                "the connecting process is not running inside this session",
            )
            .with_data(json!({ "peer_pid": self.peer.pid })));
        }
        self.identity = Identity::Session(p.session_id.clone());
        Ok(json!({ "session": self.shared.directory.session(&p.session_id) }))
    }

    fn own_session(&self) -> Result<String, RpcError> {
        match &self.identity {
            Identity::Session(id) => {
                if self.shared.directory.session_auth(id).is_none() {
                    return Err(RpcError::new(code::SESSION_NOT_FOUND, "session no longer exists"));
                }
                Ok(id.clone())
            }
            Identity::None => {
                Err(RpcError::new(code::UNAUTHENTICATED, "call session.attach first"))
            }
            Identity::Controller(_) => Err(RpcError::new(
                code::FORBIDDEN,
                "only processes inside a session may report for it",
            )),
        }
    }

    fn report_state(&mut self, p: ReportStateParams) -> Result<Value, RpcError> {
        let id = self.own_session()?;
        if let Some(agent) = &p.agent {
            text::check_ident("agent", agent, 64).map_err(invalid)?;
        }
        if let Some(msg) = &p.msg {
            text::check_display("msg", msg, text::MAX_MSG_CHARS).map_err(invalid)?;
        }
        let report = StateReport { state: p.state, agent: p.agent, msg: p.msg };
        self.shared.directory.report_state(&id, &report).map_err(dir_err)?;
        Ok(json!({}))
    }

    fn notify(&mut self, p: NotifyParams) -> Result<Value, RpcError> {
        let id = self.own_session()?;
        text::check_display("title", &p.title, text::MAX_TITLE_CHARS).map_err(invalid)?;
        text::check_display("body", &p.body, text::MAX_BODY_CHARS).map_err(invalid)?;
        if p.title.is_empty() {
            return Err(invalid("title must not be empty"));
        }
        self.shared.directory.notify(&id, &p.title, &p.body).map_err(dir_err)?;
        Ok(json!({}))
    }

    fn pair(&mut self, p: PairParams) -> Result<Value, RpcError> {
        self.require_anonymous()?;
        text::check_display("client_name", &p.client_name, 64).map_err(invalid)?;
        if p.client_name.trim().is_empty() {
            return Err(invalid("client_name must not be empty"));
        }
        let requested: BTreeSet<Scope> = p.scopes.into_iter().collect();
        if requested.is_empty() {
            return Err(invalid("request at least one scope"));
        }
        if self.pair_attempts >= self.shared.limits.max_pair_attempts {
            return Err(RpcError::new(code::RATE_LIMITED, "too many pairing attempts"));
        }
        self.pair_attempts += 1;
        let request = PairingRequest {
            client_name: p.client_name.clone(),
            scopes: requested.clone(),
            peer: self.peer.clone(),
        };
        let decision = {
            let _one_dialog = lock(&self.shared.dialog);
            self.shared.approver.approve_pairing(&request)
        };
        let (scopes, always) = match decision {
            PairingDecision::Deny => {
                return Err(RpcError::new(code::DENIED, "the user declined the pairing"));
            }
            PairingDecision::Approve { scopes, always_allow_input } => (
                scopes.intersection(&requested).copied().collect::<BTreeSet<_>>(),
                always_allow_input,
            ),
        };
        if scopes.is_empty() {
            return Err(RpcError::new(code::DENIED, "the user approved no scopes"));
        }
        let (grant, token) = lock(&self.shared.grants)
            .create(&p.client_name, scopes, always)
            .map_err(|e| RpcError::new(code::INTERNAL_ERROR, format!("saving the grant: {e}")))?;
        self.identity = Identity::Controller(grant.id.clone());
        Ok(json!({
            "grant_id": grant.id,
            "token": token,
            "scopes": grant.scopes,
            "always_allow_input": grant.always_allow_input,
        }))
    }

    fn authenticate(&mut self, p: AuthenticateParams) -> Result<Value, RpcError> {
        self.require_anonymous()?;
        let grant = lock(&self.shared.grants)
            .authenticate(&p.token)
            .ok_or_else(|| RpcError::new(code::UNAUTHENTICATED, "unknown or revoked token"))?;
        self.identity = Identity::Controller(grant.id.clone());
        Ok(json!({
            "grant_id": grant.id,
            "client_name": grant.client_name,
            "scopes": grant.scopes,
            "always_allow_input": grant.always_allow_input,
        }))
    }

    fn unpair(&mut self) -> Result<Value, RpcError> {
        let Identity::Controller(id) = &self.identity else {
            return Err(RpcError::new(code::UNAUTHENTICATED, "not authenticated as a controller"));
        };
        lock(&self.shared.grants)
            .revoke(id)
            .map_err(|e| RpcError::new(code::INTERNAL_ERROR, format!("revoking: {e}")))?;
        self.identity = Identity::None;
        Ok(json!({}))
    }

    /// The current grant (re-read on every request, so revocation is immediate), checked
    /// for `scope`.
    fn grant_with(&self, scope: Scope) -> Result<Grant, RpcError> {
        let id = match &self.identity {
            Identity::Controller(id) => id,
            Identity::None => {
                return Err(RpcError::new(
                    code::UNAUTHENTICATED,
                    "call halyard.authenticate or halyard.pair first",
                ));
            }
            Identity::Session(_) => {
                return Err(RpcError::new(
                    code::FORBIDDEN,
                    "session connections cannot use controller methods",
                ));
            }
        };
        let grant = lock(&self.shared.grants)
            .get(id)
            .cloned()
            .ok_or_else(|| RpcError::new(code::UNAUTHENTICATED, "this grant was revoked"))?;
        if !grant.has(scope) {
            return Err(RpcError::new(code::FORBIDDEN, format!("missing scope {scope}"))
                .with_data(json!({ "required_scope": scope })));
        }
        Ok(grant)
    }

    fn send_text(&mut self, p: SendTextParams) -> Result<Value, RpcError> {
        let grant = self.grant_with(Scope::InputWrite)?;
        if p.text.is_empty() {
            return Err(invalid("text must not be empty"));
        }
        if p.text.len() > text::MAX_SEND_TEXT_BYTES {
            return Err(invalid(format!("text exceeds {} bytes", text::MAX_SEND_TEXT_BYTES)));
        }
        let session = self
            .shared
            .directory
            .session(&p.session_id)
            .ok_or_else(|| RpcError::new(code::SESSION_NOT_FOUND, "session not found"))?;
        let reason = if text::needs_fresh_approval(&p.text) {
            Some(SendTextReason::ControlCharacters)
        } else if !grant.always_allow_input {
            Some(SendTextReason::NoStandingPermission)
        } else {
            None
        };
        if let Some(reason) = reason {
            let request = SendTextRequest {
                grant_id: grant.id.clone(),
                client_name: grant.client_name.clone(),
                session,
                display_text: text::escape_for_display(&p.text),
                text: p.text.clone(),
                reason,
                peer: self.peer.clone(),
            };
            let approved = {
                let _one_dialog = lock(&self.shared.dialog);
                self.shared.approver.approve_send_text(&request)
            };
            if !approved {
                return Err(RpcError::new(code::DENIED, "the user declined the input"));
            }
        }
        self.shared.directory.send_text(&p.session_id, &p.text).map_err(dir_err)?;
        let approval = if reason.is_some() { "user" } else { "standing" };
        Ok(json!({ "sent": true, "approval": approval }))
    }

    fn subscribe(&mut self) -> Result<Value, RpcError> {
        self.grant_with(Scope::EventsSubscribe)?;
        if self.subscription.is_some() {
            return Ok(json!({ "subscribed": true }));
        }
        let (sub_id, rx) = self.shared.events.subscribe(self.shared.limits.event_queue);
        let writer = Arc::clone(&self.writer);
        let stream = self
            .stream
            .try_clone()
            .map_err(|e| RpcError::new(code::INTERNAL_ERROR, format!("subscribing: {e}")))?;
        let grants = Arc::clone(&self.shared.grants);
        let Identity::Controller(grant_id) = self.identity.clone() else {
            return Err(RpcError::new(code::INTERNAL_ERROR, "identity changed"));
        };
        let spawned = thread::Builder::new().name("halyard-ipc-events".into()).spawn(move || {
            for event in rx {
                // Revocation stops the stream too, not only request handling.
                let still_granted =
                    lock(&grants).get(&grant_id).is_some_and(|g| g.has(Scope::EventsSubscribe));
                if !still_granted {
                    break;
                }
                if writer.send(&rpc::notification(event.method(), event.params())).is_err() {
                    break;
                }
            }
            // The stream ended: the connection closed, the grant was revoked, or this
            // subscriber fell behind and was dropped. Close so the client notices.
            let _ = stream.shutdown(std::net::Shutdown::Both);
        });
        if let Err(e) = spawned {
            self.shared.events.unsubscribe(sub_id);
            return Err(RpcError::new(code::INTERNAL_ERROR, format!("subscribing: {e}")));
        }
        self.subscription = Some(sub_id);
        Ok(json!({ "subscribed": true }))
    }
}
