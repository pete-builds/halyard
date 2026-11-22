//! The session API inside the app: the pane registry the server reads, the
//! [`SessionDirectory`] and [`Approver`] it calls, and the events it publishes.
//!
//! Nothing here touches GPUI. The server calls the directory and the approver on
//! its connection threads; everything those calls need is either thread safe
//! already (a pane's `Session` is a set of channels, its monitor a mutex) or is
//! handed to the UI thread as a [`UiRequest`] over an unbounded channel. A call
//! that needs an answer from the UI (focus, a dialog) waits on its own reply
//! channel with a timeout, so a busy or closed window can never hold a
//! connection thread forever, and a connection thread never blocks the UI.
//!
//! The protocol is `docs/session-api.md`; `crate::session_api_ui` draws the
//! dialogs and runs the UI half.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant, SystemTime};

use futures::channel::mpsc::UnboundedSender;
use halyard_agents::{AgentKind, AttentionState, Cause};
use halyard_ipc::{
    AgentState, Approver, DirectoryError, EventHub, IpcServer, PairingDecision, PairingRequest,
    SendTextRequest, SessionAuth, SessionDirectory, SessionEvent, SessionInfo, SessionToken,
    StateReport, TokenHash,
};
use halyard_term::Session;

use crate::signals::{self, SharedMonitor};

/// How long a dialog waits for the user before it counts as a denial.
pub const DIALOG_TIMEOUT: Duration = Duration::from_secs(90);

/// How long a focus request waits for the UI thread.
pub const FOCUS_TIMEOUT: Duration = Duration::from_secs(2);

/// Largest screen text returned by `session.read_screen`, in bytes.
pub const MAX_SCREEN_BYTES: usize = 256 * 1024;

/// Pairing dialogs allowed per [`PAIRING_WINDOW`], across all connections.
pub const MAX_PAIRING_PROMPTS: usize = 3;
/// The window [`MAX_PAIRING_PROMPTS`] counts over.
pub const PAIRING_WINDOW: Duration = Duration::from_secs(60);
/// After the user denies a pairing, further pairing requests are refused without a
/// dialog for this long.
pub const PAIRING_COOLDOWN: Duration = Duration::from_secs(30);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What the server needs to know about one live pane.
#[derive(Debug, Clone)]
pub struct PaneEntry {
    /// The pane's session. Weak, so the registry never keeps a closed pane's shell
    /// alive.
    pub session: Weak<Session>,
    /// The pane's signals, shared with its session thread and the UI.
    pub monitor: SharedMonitor,
    /// The PTY child's pid, for the ancestry check.
    pub child_pid: u32,
    /// Hash of the pane's `HALYARD_SESSION_TOKEN`.
    pub token_hash: TokenHash,
}

/// Every live pane by `HALYARD_SESSION_ID`, shared by the UI and the server's
/// connection threads. Each call holds the lock only to copy an entry out.
#[derive(Debug, Clone, Default)]
pub struct Registry(Arc<Mutex<HashMap<String, PaneEntry>>>);

impl Registry {
    /// Adds a pane.
    pub fn insert(&self, id: &str, entry: PaneEntry) {
        lock(&self.0).insert(id.to_owned(), entry);
    }

    /// Removes a pane. Returns whether it was there.
    pub fn remove(&self, id: &str) -> bool {
        lock(&self.0).remove(id).is_some()
    }

    /// A copy of one pane's entry.
    pub fn get(&self, id: &str) -> Option<PaneEntry> {
        lock(&self.0).get(id).cloned()
    }

    /// The pane's session, if the pane is registered and its session alive.
    fn live(&self, id: &str) -> Option<(Arc<Session>, PaneEntry)> {
        let entry = self.get(id)?;
        Some((entry.session.upgrade()?, entry))
    }

    fn ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = lock(&self.0).keys().cloned().collect();
        ids.sort();
        ids
    }
}

/// What a pane needs to join the API: the variables for its child's environment
/// and its token, made before the child starts.
#[derive(Debug)]
pub struct PaneCredentials {
    /// `HALYARD_SOCKET`, `HALYARD_SESSION_ID`, `HALYARD_SESSION_TOKEN`.
    pub env: Vec<(String, String)>,
    /// The token; only its hash is kept once the pane is registered.
    pub token: SessionToken,
}

/// The environment for a new pane with id `session_id`, or `None` when the socket
/// path is not valid UTF-8 (the PTY layer takes `String` variables).
pub fn pane_credentials(socket: &std::path::Path, session_id: &str) -> Option<PaneCredentials> {
    let token = SessionToken::generate();
    let env = halyard_ipc::session_env(socket, session_id, &token)
        .into_iter()
        .map(|(k, v)| v.into_string().ok().map(|v| (k.to_owned(), v)))
        .collect::<Option<Vec<_>>>()?;
    Some(PaneCredentials { env, token })
}

/// The running server, once bound. Empty before the bind finishes, after a failed
/// bind, and after shutdown.
pub type ServerSlot = Arc<Mutex<Option<Arc<IpcServer>>>>;

/// What each pane holds to take part in the API. Cheap to clone.
#[derive(Debug, Clone)]
pub struct PaneApi {
    socket: PathBuf,
    registry: Registry,
    server: ServerSlot,
    publisher: Arc<Mutex<EventPublisher>>,
    events: EventHub,
}

impl PaneApi {
    /// A link to the API at `socket`.
    pub fn new(
        socket: PathBuf,
        registry: Registry,
        server: ServerSlot,
        publisher: Arc<Mutex<EventPublisher>>,
        events: EventHub,
    ) -> Self {
        Self { socket, registry, server, publisher, events }
    }

    /// The socket path the panes are told about.
    #[must_use]
    pub fn socket(&self) -> &std::path::Path {
        &self.socket
    }

    /// Takes the server out, so dropping it stops it and removes the socket.
    pub fn take_server(&self) -> Option<Arc<IpcServer>> {
        lock(&self.server).take()
    }

    /// The server, if it is running.
    #[must_use]
    pub fn server(&self) -> Option<Arc<IpcServer>> {
        lock(&self.server).clone()
    }

    /// Registers a started pane. Returns false (and registers nothing) when the
    /// platform reported no child pid, since the ancestry check needs one.
    pub fn register(
        &self,
        session: &Arc<Session>,
        monitor: &SharedMonitor,
        token: &SessionToken,
    ) -> bool {
        let Some(child_pid) = session.pid() else { return false };
        self.registry.insert(
            session.id(),
            PaneEntry {
                session: Arc::downgrade(session),
                monitor: Arc::clone(monitor),
                child_pid,
                token_hash: token.hash(),
            },
        );
        true
    }

    /// The pane's program exited or the pane closed: unregister it and tell
    /// subscribers.
    pub fn closed(&self, session_id: &str, exit_code: Option<i32>) {
        if self.registry.remove(session_id) {
            let event = lock(&self.publisher).exited(session_id, exit_code);
            self.events.publish(event);
        }
    }

    /// The user pressed a line-ending or line-clearing key in the pane (see
    /// [`crate::keys::resets_typed_line`]). Only real GUI key events call this.
    pub fn user_line_key(&self, session_id: &str) {
        if let Some(server) = self.server() {
            server.reset_typed_since_approval(session_id);
        }
    }

    /// Publishes what changed about a pane's attention.
    pub fn observe(&self, o: Observed<'_>) {
        let events = lock(&self.publisher).observe(o);
        for e in events {
            self.events.publish(e);
        }
    }
}

/// The environment changes for a pane's child: always remove an inherited
/// `HALYARD_SOCKET` and `HALYARD_SESSION_TOKEN` (Halyard started from another
/// Halyard's pane must not report into the outer one), then set this pane's
/// variables when it has credentials. Returns (set, remove).
#[must_use]
pub fn pane_env(creds: Option<&PaneCredentials>) -> (Vec<(String, String)>, Vec<String>) {
    let remove =
        vec![halyard_ipc::ENV_SOCKET.to_owned(), halyard_ipc::ENV_SESSION_TOKEN.to_owned()];
    (creds.map(|c| c.env.clone()).unwrap_or_default(), remove)
}

/// A request only the UI thread can answer or act on.
#[derive(Debug)]
pub enum UiRequest {
    /// Something to tell the user once, in a banner (the server could not start).
    Notice(String),
    /// A pane's attention changed through the API: redraw its marks now instead
    /// of at the next poll.
    Changed {
        /// The pane.
        session_id: String,
    },
    /// Bring a pane to the front.
    Focus {
        /// The pane.
        session_id: String,
        /// Where the answer goes.
        reply: SyncSender<Result<(), DirectoryError>>,
    },
    /// Ask the user to pair a tool.
    Pairing {
        /// What the tool asked for.
        request: PairingRequest,
        /// Where the answer goes. Dropping it without an answer is a denial.
        reply: SyncSender<PairingDecision>,
    },
    /// Ask the user to let a tool type into a pane.
    SendText {
        /// What the tool wants to type, and where.
        request: Box<SendTextRequest>,
        /// Where the answer goes. Dropping it without an answer is a denial.
        reply: SyncSender<bool>,
    },
}

/// Sends requests to the UI thread.
pub type UiSender = UnboundedSender<UiRequest>;

/// The wire name of an agent kind, as `sessions.list` reports it.
#[must_use]
pub fn agent_id(kind: &AgentKind) -> Option<String> {
    let id = match kind {
        AgentKind::ClaudeCode => "claude-code",
        AgentKind::Codex => "codex",
        AgentKind::OpenCode => "opencode",
        AgentKind::GeminiCli => "gemini-cli",
        AgentKind::Aider => "aider",
        AgentKind::Goose => "goose",
        AgentKind::Amp => "amp",
        AgentKind::Other(name) => {
            return halyard_ipc::text::check_ident("agent", name, 64).ok().map(|()| name.clone());
        }
    };
    Some(id.to_owned())
}

/// The protocol state for an attention state; `None` while nothing is known.
#[must_use]
pub fn wire_state(state: AttentionState) -> Option<AgentState> {
    match state {
        AttentionState::Unknown => None,
        AttentionState::Working => Some(AgentState::Working),
        AttentionState::WaitingForInput => Some(AgentState::Waiting),
        AttentionState::Idle => Some(AgentState::Idle),
        AttentionState::Finished { .. } => Some(AgentState::Done),
        AttentionState::Error => Some(AgentState::Error),
    }
}

/// The protocol state for a report, in the scanner's vocabulary.
fn reported(state: AgentState) -> halyard_shell::ReportedState {
    use halyard_shell::ReportedState as R;
    match state {
        AgentState::Working => R::Working,
        AgentState::Waiting => R::Waiting,
        AgentState::Idle => R::Idle,
        AgentState::Done => R::Done,
        AgentState::Error => R::Error,
    }
}

/// Turns what the UI observes about panes into [`SessionEvent`]s, once per change.
///
/// Shared by the UI (which calls [`EventPublisher::observe`] whenever a pane's
/// attention changed, whatever the source) and the directory (which publishes API
/// notifications itself and marks them, so they are not sent twice).
#[derive(Debug, Default)]
pub struct EventPublisher {
    last: HashMap<String, (Option<AgentState>, Option<String>)>,
}

/// What [`EventPublisher::observe`] needs about one pane.
#[derive(Debug, Clone, Copy)]
pub struct Observed<'a> {
    /// The pane's id.
    pub session_id: &'a str,
    /// Its attention state.
    pub state: AttentionState,
    /// What put it there.
    pub cause: Cause,
    /// The agent, if known.
    pub agent: Option<&'a AgentKind>,
    /// The latest notification or report message, sanitized by the monitor.
    pub note: Option<&'a str>,
}

impl EventPublisher {
    /// The events for a pane's current attention: a state change when the
    /// protocol state differs from the last one published, and a notification
    /// when a new notification text arrived (cause [`Cause::Notification`]).
    pub fn observe(&mut self, o: Observed<'_>) -> Vec<SessionEvent> {
        let mut out = Vec::new();
        let state = wire_state(o.state);
        let entry = self.last.entry(o.session_id.to_owned()).or_default();
        if let Some(s) = state
            && entry.0 != Some(s)
        {
            out.push(SessionEvent::StateChanged {
                session_id: o.session_id.to_owned(),
                state: s,
                agent: o.agent.and_then(agent_id),
                msg: (o.cause == Cause::ProtocolReport)
                    .then(|| o.note.map(str::to_owned))
                    .flatten(),
            });
        }
        entry.0 = state;
        let note = o.note.map(str::to_owned);
        if o.cause == Cause::Notification
            && note.is_some()
            && entry.1 != note
            && let Some(title) = &note
        {
            out.push(SessionEvent::Notification {
                session_id: o.session_id.to_owned(),
                title: title.clone(),
                body: String::new(),
            });
        }
        entry.1 = note;
        out
    }

    /// Records a note as already published (the API path publishes its own
    /// notification event with title and body kept apart).
    pub fn mark_note(&mut self, session_id: &str, note: Option<&str>) {
        self.last.entry(session_id.to_owned()).or_default().1 = note.map(str::to_owned);
    }

    /// The pane is gone: its exit event, and its history forgotten.
    pub fn exited(&mut self, session_id: &str, exit_code: Option<i32>) -> SessionEvent {
        self.last.remove(session_id);
        SessionEvent::Exited { session_id: session_id.to_owned(), exit_code }
    }
}

/// The working directory to show for a pane: the OSC 7 report when it names this
/// machine, else the shell's own directory. Display only.
#[must_use]
pub fn pane_cwd(session: &Session) -> Option<String> {
    let frame = session.latest_frame();
    if let Some(p) = frame.cwd.as_deref().and_then(|raw| {
        halyard_workspace::persist::osc7_local_path(raw, crate::app_state::hostname())
    }) {
        return Some(p);
    }
    let dir = session.pid().and_then(crate::procinfo::cwd)?;
    dir.to_str().and_then(halyard_workspace::persist::valid_cwd).map(str::to_owned)
}

/// How a dialog names a pane: its title, else its folder, plus the id that
/// `sessions.list` shows. Every part is untrusted text, quoted.
#[must_use]
pub fn pane_label(info: &SessionInfo) -> String {
    let title = info.title.trim();
    let name = if title.is_empty() {
        info.cwd.as_deref().map_or_else(|| "a pane".to_owned(), |c| format!("the pane in {c}"))
    } else {
        format!("\u{201c}{title}\u{201d}")
    };
    format!("{name} (id {})", info.id)
}

/// Cuts `text` to at most `max` bytes on a character boundary.
fn cap(mut text: String, max: usize) -> String {
    if text.len() > max {
        let mut cut = max;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
    }
    text
}

/// The app's [`SessionDirectory`]: the registry, plus the UI channel for the
/// calls that need the UI thread.
#[derive(Debug)]
pub struct AppDirectory {
    registry: Registry,
    ui: UiSender,
    events: EventHub,
    publisher: Arc<Mutex<EventPublisher>>,
}

impl AppDirectory {
    /// A directory over `registry` that hands UI work to `ui` and publishes to
    /// `events`.
    pub fn new(
        registry: Registry,
        ui: UiSender,
        events: EventHub,
        publisher: Arc<Mutex<EventPublisher>>,
    ) -> Self {
        Self { registry, ui, events, publisher }
    }

    fn info(id: &str, session: &Session, entry: &PaneEntry) -> SessionInfo {
        let (state, agent) = {
            let m = signals::lock(&entry.monitor);
            let a = m.attention();
            (wire_state(a.state()), a.agent().and_then(agent_id))
        };
        SessionInfo {
            id: id.to_owned(),
            title: session.latest_frame().title.clone(),
            cwd: pane_cwd(session),
            agent,
            state,
        }
    }

    fn changed(&self, id: &str) {
        // The UI is gone only while the app quits; nothing to redraw then.
        self.ui.unbounded_send(UiRequest::Changed { session_id: id.to_owned() }).ok();
    }
}

impl SessionDirectory for AppDirectory {
    fn list_sessions(&self) -> Vec<SessionInfo> {
        self.registry
            .ids()
            .into_iter()
            .filter_map(|id| {
                let (session, entry) = self.registry.live(&id)?;
                Some(Self::info(&id, &session, &entry))
            })
            .collect()
    }

    fn session(&self, id: &str) -> Option<SessionInfo> {
        let (session, entry) = self.registry.live(id)?;
        Some(Self::info(id, &session, &entry))
    }

    fn session_auth(&self, id: &str) -> Option<SessionAuth> {
        let (_, entry) = self.registry.live(id)?;
        Some(SessionAuth { child_pid: entry.child_pid, token_hash: entry.token_hash })
    }

    fn report_state(&self, id: &str, report: &StateReport) -> Result<(), DirectoryError> {
        let (_, entry) = self.registry.live(id).ok_or(DirectoryError::NotFound)?;
        let report = halyard_shell::AgentReport {
            state: Some(reported(report.state)),
            agent: report.agent.clone(),
            msg: report.msg.clone(),
            session: None,
            exit_code: None,
        };
        signals::lock(&entry.monitor).on_api_report(report, SystemTime::now());
        self.changed(id);
        Ok(())
    }

    fn notify(&self, id: &str, title: &str, body: &str) -> Result<(), DirectoryError> {
        let (_, entry) = self.registry.live(id).ok_or(DirectoryError::NotFound)?;
        let text = if body.is_empty() { title.to_owned() } else { format!("{title}: {body}") };
        let note = {
            let mut m = signals::lock(&entry.monitor);
            m.on_api_notification(&text, SystemTime::now());
            m.attention().last_notification().map(str::to_owned)
        };
        lock(&self.publisher).mark_note(id, note.as_deref());
        self.events.publish(SessionEvent::Notification {
            session_id: id.to_owned(),
            title: title.to_owned(),
            body: body.to_owned(),
        });
        self.changed(id);
        Ok(())
    }

    fn focus(&self, id: &str) -> Result<(), DirectoryError> {
        if self.registry.live(id).is_none() {
            return Err(DirectoryError::NotFound);
        }
        let (reply, answer) = sync_channel(1);
        self.ui
            .unbounded_send(UiRequest::Focus { session_id: id.to_owned(), reply })
            .map_err(|_| DirectoryError::Unavailable("Halyard is shutting down".into()))?;
        answer.recv_timeout(FOCUS_TIMEOUT).unwrap_or_else(|_| {
            Err(DirectoryError::Unavailable("the window did not answer".into()))
        })
    }

    fn read_screen(&self, id: &str) -> Result<String, DirectoryError> {
        let (session, _) = self.registry.live(id).ok_or(DirectoryError::NotFound)?;
        Ok(cap(session.latest_frame().text(), MAX_SCREEN_BYTES))
    }

    fn send_text(&self, id: &str, text: &str) -> Result<(), DirectoryError> {
        let (session, _) = self.registry.live(id).ok_or(DirectoryError::NotFound)?;
        // The paste path: control bytes the terminal treats as unsafe are
        // neutralized, and the text is bracketed when the program asked for it.
        // This is not user typing, so it never resets `typed_since_approval`
        // (only real key events in the pane do; see `keys::resets_typed_line`).
        session.paste(text);
        Ok(())
    }
}

/// The "Paired Tools" view's model: the grants as the server has them, with a
/// label per row, and revoking by row.
#[derive(Debug, Clone, Default)]
pub struct PairedTools {
    grants: Vec<halyard_ipc::Grant>,
}

/// "5 min ago" for a Unix time, relative to `now`.
#[must_use]
pub fn ago(then: u64, now: SystemTime) -> String {
    let now = now.duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let secs = now.saturating_sub(then);
    match secs {
        0..60 => "just now".to_owned(),
        60..3600 => format!("{} min ago", secs / 60),
        3600..86_400 => format!("{} h ago", secs / 3600),
        _ => format!("{} days ago", secs / 86_400),
    }
}

impl PairedTools {
    /// A fresh read of the grants.
    #[must_use]
    pub fn load(handle: &halyard_ipc::GrantsHandle) -> Self {
        Self { grants: handle.list() }
    }

    /// One row per grant: name, scopes, "always allow typing", last use.
    #[must_use]
    pub fn labels(&self, now: SystemTime) -> Vec<String> {
        self.grants
            .iter()
            .map(|g| {
                let scopes: Vec<&str> = g.scopes.iter().map(|s| s.as_str()).collect();
                let used = g.last_used.map_or_else(
                    || "never used".to_owned(),
                    |t| format!("last used {}", ago(t, now)),
                );
                let always = if g.always_allow_input { " · types without asking" } else { "" };
                let id: String = g.id.chars().take(6).collect();
                format!("{} · {}{always} · {used} · id {id}", g.client_name, scopes.join(", "))
            })
            .collect()
    }

    /// Revokes the grant on row `index`. Returns its client name, or `None` when
    /// the row does not exist or the grant was already gone.
    pub fn revoke(
        &self,
        handle: &halyard_ipc::GrantsHandle,
        index: usize,
    ) -> Result<Option<String>, halyard_ipc::Error> {
        let Some(g) = self.grants.get(index) else { return Ok(None) };
        Ok(handle.revoke(&g.id)?.then(|| g.client_name.clone()))
    }
}

/// Limits pairing dialogs across all connections, on top of the server's own
/// limits (three attempts per connection, one dialog at a time).
#[derive(Debug, Default)]
pub struct PairingLimiter {
    shown: VecDeque<Instant>,
    blocked_until: Option<Instant>,
}

impl PairingLimiter {
    /// Whether a pairing dialog may be shown now. Counts it when it may.
    pub fn allow(&mut self, now: Instant) -> bool {
        if self.blocked_until.is_some_and(|t| now < t) {
            return false;
        }
        while self.shown.front().is_some_and(|t| now.duration_since(*t) >= PAIRING_WINDOW) {
            self.shown.pop_front();
        }
        if self.shown.len() >= MAX_PAIRING_PROMPTS {
            return false;
        }
        self.shown.push_back(now);
        true
    }

    /// The user denied a pairing: refuse further ones for a while.
    pub fn denied(&mut self, now: Instant) {
        self.blocked_until = Some(now + PAIRING_COOLDOWN);
    }
}

/// The app's [`Approver`]: every question becomes a dialog on the UI thread, and
/// no answer within [`DIALOG_TIMEOUT`] is a denial.
#[derive(Debug)]
pub struct DialogApprover {
    ui: UiSender,
    timeout: Duration,
    limiter: Mutex<PairingLimiter>,
}

impl DialogApprover {
    /// An approver that asks through `ui`, waiting at most `timeout` per dialog.
    pub fn new(ui: UiSender, timeout: Duration) -> Self {
        Self { ui, timeout, limiter: Mutex::new(PairingLimiter::default()) }
    }
}

impl Approver for DialogApprover {
    fn approve_pairing(&self, request: &PairingRequest) -> PairingDecision {
        if !lock(&self.limiter).allow(Instant::now()) {
            return PairingDecision::Deny;
        }
        let (reply, answer) = sync_channel(1);
        if self.ui.unbounded_send(UiRequest::Pairing { request: request.clone(), reply }).is_err() {
            return PairingDecision::Deny;
        }
        let decision = answer.recv_timeout(self.timeout).unwrap_or(PairingDecision::Deny);
        if decision == PairingDecision::Deny {
            lock(&self.limiter).denied(Instant::now());
        }
        decision
    }

    fn approve_send_text(&self, request: &SendTextRequest) -> bool {
        // The server refuses these before asking (`too_long_to_review`); this keeps
        // the rule in the app too: never ask about text the dialog cannot show.
        if request.display_text.len() > crate::approval::MAX_DIALOG_TEXT_BYTES {
            return false;
        }
        let (reply, answer) = sync_channel(1);
        if self
            .ui
            .unbounded_send(UiRequest::SendText { request: Box::new(request.clone()), reply })
            .is_err()
        {
            return false;
        }
        answer.recv_timeout(self.timeout).unwrap_or(false)
    }
}

#[cfg(test)]
mod tests;
