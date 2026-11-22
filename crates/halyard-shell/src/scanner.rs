//! Incremental scanner for terminal signals in raw PTY output.
//!
//! The scanner is a tap: it reads the same bytes the VT engine reads, never changes
//! them, and reports the few sequences Halyard cares about as [`ShellEvent`]s. Chunks
//! may be split anywhere, including in the middle of an escape sequence or a UTF-8
//! character; the scanner keeps its state between calls to [`Scanner::feed`].
//!
//! The parse follows the shape of a VT500 style parser (ground, escape, CSI, OSC and
//! string states) closely enough to stay in step with the engine:
//!
//! * OSC ends with BEL or ST (`ESC \`). An ESC followed by anything else aborts the OSC
//!   and starts a new escape sequence, as xterm does. CAN and SUB abort it.
//! * An OSC longer than the configured cap is dropped: the scanner keeps consuming
//!   until the terminator and then resumes in ground state.
//! * DCS, SOS, PM and APC strings are skipped until ST, so a BEL inside them is not a
//!   bell.
//!
//! # Locating events
//!
//! [`Scanner::feed`] reports, with each event, the offset in the chunk just past the
//! byte that completed it. A caller that needs the cursor row of a mark (the block
//! model does) feeds the engine up to that offset, reads the cursor, and continues.
//!
//! # Trust
//!
//! All input is untrusted. Every string in an event is percent or escape decoded
//! where the format says so, then stripped of control characters and bidi overrides
//! and length capped (see [`crate::sanitize::limits`]). Nothing beyond the documented
//! fields is interpreted.

use crate::sanitize::{hex_val, limits, percent_decode, sanitize_bytes};

/// Default cap on the payload length of one OSC sequence, in bytes.
pub const DEFAULT_MAX_OSC_LEN: usize = 8192;

/// The OSC number Halyard uses for its session protocol. See `docs/protocol.md`.
pub const HALYARD_OSC: u32 = 7717;

/// Longest CSI parameter string the scanner keeps. Longer ones are skipped; they can
/// not be one of the few mode switches the scanner looks at.
const MAX_CSI_LEN: usize = 32;

const BEL: u8 = 0x07;
const ESC: u8 = 0x1b;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;

/// A signal extracted from terminal output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellEvent {
    /// OSC 133;A or 633;A: the prompt is about to be drawn.
    PromptStart {
        /// The `aid=` option, when present. Shells nested inside one another use
        /// different ids, which lets the block model tell a nested shell from a lost
        /// end-of-command mark.
        aid: Option<String>,
    },
    /// OSC 133;B or 633;B: the prompt is drawn and the user's input starts here.
    CommandInputStart,
    /// OSC 133;C or 633;C: the command line was accepted; its output starts here.
    CommandExecuted,
    /// OSC 133;D or 633;D: the command finished.
    CommandFinished {
        /// The exit status, when the mark carries a numeric one.
        exit_code: Option<i32>,
        /// The `aid=` option, when present.
        aid: Option<String>,
    },
    /// OSC 633;E, or the `cmdline_url=` option of OSC 133;C: the command line text.
    CommandLine(String),
    /// OSC 7 (`file://host/path`), or OSC 633;P;Cwd=path.
    Cwd(ReportedCwd),
    /// OSC 9 (iTerm2 style) or OSC 777;notify desktop notification.
    Notification {
        /// Which sequence carried it.
        source: NotificationSource,
        /// Title, for OSC 777 only.
        title: Option<String>,
        /// Notification body.
        body: String,
    },
    /// OSC 9;4 progress report (ConEmu, Windows Terminal, Ghostty).
    Progress {
        /// Progress state.
        state: ProgressState,
        /// Percentage, clamped to 0 to 100, when given.
        percent: Option<u8>,
    },
    /// OSC 0, 1 or 2. Informational only.
    Title {
        /// Which title was set.
        kind: TitleKind,
        /// The new text.
        text: String,
    },
    /// A BEL that rang: in ground state, or executed inside an unfinished escape or
    /// CSI sequence as a VT parser does. A BEL that terminates an OSC, or sits inside
    /// a DCS or APC string, is not a bell.
    Bell,
    /// The alternate screen was entered or left (CSI ? 1049, 1047 or 47 h/l).
    AltScreen {
        /// True when the alternate screen became active.
        active: bool,
    },
    /// A Halyard session protocol report (OSC 7717, or OSC 777;halyard).
    Agent(AgentReport),
}

/// A working directory reported by the shell (OSC 7 or OSC 633;P).
///
/// The path is sanitized for display. A path that sanitizing changed (control
/// characters, bidi overrides, invalid UTF-8, or more than the length cap) is a
/// different path, so it is kept for display only: [`ReportedCwd::verbatim`] is false
/// and [`ReportedCwd::local_path`] returns `None`. The same goes for a host that is
/// not this machine, which is the normal case inside `ssh`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportedCwd {
    /// Host name, if the URI had one.
    pub host: Option<String>,
    /// Decoded absolute path, sanitized for display.
    pub path: String,
    /// True when `path` and `host` are exactly what the sequence encoded.
    pub verbatim: bool,
}

impl ReportedCwd {
    /// A local cwd with no host, as the tests and OSC 633;P produce.
    pub fn local(path: impl Into<String>) -> Self {
        Self { host: None, path: path.into(), verbatim: true }
    }

    /// True when the host is absent, `localhost`, or names this machine
    /// (`local_hostname`, compared without case, or by its first label).
    pub fn is_local_host(&self, local_hostname: &str) -> bool {
        let Some(host) = self.host.as_deref() else {
            return true;
        };
        let short = |h: &str| h.split('.').next().unwrap_or(h).to_ascii_lowercase();
        host.eq_ignore_ascii_case("localhost")
            || host.eq_ignore_ascii_case(local_hostname)
            || (!local_hostname.is_empty() && short(host) == short(local_hostname))
    }

    /// The path, when it can be used as a directory on this machine (reading git
    /// state, opening a new pane there): it must be verbatim and on a local host.
    pub fn local_path(&self, local_hostname: &str) -> Option<&std::path::Path> {
        (self.verbatim && self.is_local_host(local_hostname))
            .then(|| std::path::Path::new(&self.path))
    }
}

/// Which escape sequence delivered a notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationSource {
    /// OSC 9.
    Osc9,
    /// OSC 777;notify.
    Osc777,
}

/// The state field of OSC 9;4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressState {
    /// 0: remove the progress indicator.
    Remove,
    /// 1: normal progress.
    Normal,
    /// 2: error.
    Error,
    /// 3: indeterminate.
    Indeterminate,
    /// 4: paused or warning.
    Paused,
}

/// Which title an OSC 0, 1 or 2 sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleKind {
    /// OSC 0: icon name and window title.
    Both,
    /// OSC 1: icon name only.
    Icon,
    /// OSC 2: window title only.
    Window,
}

/// A report in Halyard's session protocol. Every field is optional; a report with
/// no recognized field is not emitted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentReport {
    /// `state=`.
    pub state: Option<ReportedState>,
    /// `agent=`: the program's name for itself.
    pub agent: Option<String>,
    /// `msg=`: a short human readable message.
    pub msg: Option<String>,
    /// `session=`: the `HALYARD_SESSION_ID` the reporter saw, if it echoed it.
    pub session: Option<String>,
    /// `exit=`: an exit status that goes with `state=done` or `state=error`.
    pub exit_code: Option<i32>,
}

/// The `state=` values of the Halyard session protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportedState {
    /// `working`.
    Working,
    /// `waiting`: the program needs the user.
    Waiting,
    /// `idle`.
    Idle,
    /// `done`: the program finished its task.
    Done,
    /// `error`.
    Error,
}

impl ReportedState {
    /// Parses a protocol value. Unknown values return `None`.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "working" => Some(Self::Working),
            "waiting" => Some(Self::Waiting),
            "idle" => Some(Self::Idle),
            "done" => Some(Self::Done),
            "error" => Some(Self::Error),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    Escape,
    EscapeIntermediate,
    Csi,
    Osc,
    OscEsc,
    Str,
    StrEsc,
}

/// Incremental scanner. One per PTY.
#[derive(Debug)]
pub struct Scanner {
    state: State,
    osc: Vec<u8>,
    osc_overflow: bool,
    max_osc_len: usize,
    csi: [u8; MAX_CSI_LEN],
    csi_len: usize,
    csi_overflow: bool,
}

impl Default for Scanner {
    fn default() -> Self {
        Self::new()
    }
}

impl Scanner {
    /// A scanner with the default OSC length cap.
    pub fn new() -> Self {
        Self::with_max_osc_len(DEFAULT_MAX_OSC_LEN)
    }

    /// A scanner that drops any OSC whose payload exceeds `max` bytes.
    pub fn with_max_osc_len(max: usize) -> Self {
        Self {
            state: State::Ground,
            osc: Vec::new(),
            osc_overflow: false,
            max_osc_len: max,
            csi: [0; MAX_CSI_LEN],
            csi_len: 0,
            csi_overflow: false,
        }
    }

    /// True when the scanner is between sequences, so the next byte starts fresh.
    pub fn is_ground(&self) -> bool {
        self.state == State::Ground
    }

    /// Scans one chunk. `on_event` receives each event and the offset in `data` just
    /// past the byte that completed it.
    pub fn feed(&mut self, data: &[u8], mut on_event: impl FnMut(usize, ShellEvent)) {
        let mut i = 0;
        while i < data.len() {
            if self.state == State::Ground {
                // Fast path: skip printable text in one step.
                match data[i..].iter().position(|&b| b == ESC || b == BEL) {
                    None => return,
                    Some(skip) => i += skip,
                }
            }
            let b = data[i];
            i += 1;
            self.step(b, &mut |ev| on_event(i, ev));
        }
    }

    /// Scans one chunk and returns its events, without offsets.
    pub fn feed_collect(&mut self, data: &[u8]) -> Vec<ShellEvent> {
        let mut out = Vec::new();
        self.feed(data, |_, ev| out.push(ev));
        out
    }

    fn step(&mut self, b: u8, emit: &mut dyn FnMut(ShellEvent)) {
        match self.state {
            State::Ground => match b {
                ESC => self.state = State::Escape,
                BEL => emit(ShellEvent::Bell),
                _ => {}
            },
            State::Escape => self.escape(b, emit),
            State::EscapeIntermediate => match b {
                0x20..=0x2f => {}
                ESC => self.state = State::Escape,
                CAN | SUB => self.state = State::Ground,
                BEL => emit(ShellEvent::Bell),
                0x00..=0x1f | 0x7f => {}
                _ => self.state = State::Ground,
            },
            State::Csi => match b {
                0x20..=0x3f => {
                    if self.csi_len < MAX_CSI_LEN {
                        self.csi[self.csi_len] = b;
                        self.csi_len += 1;
                    } else {
                        self.csi_overflow = true;
                    }
                }
                0x40..=0x7e => {
                    self.state = State::Ground;
                    if !self.csi_overflow {
                        self.dispatch_csi(b, emit);
                    }
                }
                ESC => self.state = State::Escape,
                CAN | SUB => self.state = State::Ground,
                BEL => emit(ShellEvent::Bell),
                0x00..=0x1f | 0x7f => {}
                _ => self.state = State::Ground,
            },
            State::Osc => match b {
                BEL => {
                    self.state = State::Ground;
                    self.finish_osc(emit);
                }
                ESC => self.state = State::OscEsc,
                CAN | SUB => self.state = State::Ground,
                0x00..=0x1f => {}
                _ => {
                    if self.osc_overflow {
                        // Dropping: wait for the terminator.
                    } else if self.osc.len() >= self.max_osc_len {
                        self.osc_overflow = true;
                        self.osc.clear();
                    } else {
                        self.osc.push(b);
                    }
                }
            },
            State::OscEsc => {
                if b == b'\\' {
                    self.state = State::Ground;
                    self.finish_osc(emit);
                } else {
                    // ESC aborted the OSC and began a new sequence.
                    self.osc.clear();
                    self.osc_overflow = false;
                    self.state = State::Escape;
                    self.escape(b, emit);
                }
            }
            State::Str => match b {
                ESC => self.state = State::StrEsc,
                CAN | SUB => self.state = State::Ground,
                _ => {}
            },
            State::StrEsc => {
                if b == b'\\' {
                    self.state = State::Ground;
                } else {
                    self.state = State::Escape;
                    self.escape(b, emit);
                }
            }
        }
    }

    fn escape(&mut self, b: u8, emit: &mut dyn FnMut(ShellEvent)) {
        match b {
            b']' => {
                self.osc.clear();
                self.osc_overflow = false;
                self.state = State::Osc;
            }
            b'[' => {
                self.csi_len = 0;
                self.csi_overflow = false;
                self.state = State::Csi;
            }
            b'P' | b'X' | b'^' | b'_' => self.state = State::Str,
            ESC => self.state = State::Escape,
            CAN | SUB => self.state = State::Ground,
            BEL => emit(ShellEvent::Bell),
            0x20..=0x2f => self.state = State::EscapeIntermediate,
            0x00..=0x1f | 0x7f => {}
            _ => self.state = State::Ground,
        }
    }

    fn dispatch_csi(&mut self, final_byte: u8, emit: &mut dyn FnMut(ShellEvent)) {
        let active = match final_byte {
            b'h' => true,
            b'l' => false,
            _ => return,
        };
        let params = &self.csi[..self.csi_len];
        let Some(rest) = params.strip_prefix(b"?") else {
            return;
        };
        if rest.split(|&c| c == b';').any(|p| matches!(p, b"1049" | b"1047" | b"47")) {
            emit(ShellEvent::AltScreen { active });
        }
    }

    fn finish_osc(&mut self, emit: &mut dyn FnMut(ShellEvent)) {
        if self.osc_overflow {
            self.osc_overflow = false;
            self.osc.clear();
            return;
        }
        let payload = std::mem::take(&mut self.osc);
        parse_osc(&payload, emit);
        // Keep the allocation for the next sequence.
        self.osc = payload;
        self.osc.clear();
    }
}

/// Splits at the first `;`.
fn split_first(data: &[u8]) -> (&[u8], Option<&[u8]>) {
    match data.iter().position(|&b| b == b';') {
        Some(p) => (&data[..p], Some(&data[p + 1..])),
        None => (data, None),
    }
}

fn parse_number(digits: &[u8]) -> Option<u32> {
    if digits.is_empty() || digits.len() > 6 || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(digits).ok()?.parse().ok()
}

fn parse_i32(digits: &[u8]) -> Option<i32> {
    if digits.is_empty() || digits.len() > 11 {
        return None;
    }
    std::str::from_utf8(digits).ok()?.parse().ok()
}

fn parse_osc(payload: &[u8], emit: &mut dyn FnMut(ShellEvent)) {
    let (num, rest) = split_first(payload);
    let Some(num) = parse_number(num) else {
        return;
    };
    match (num, rest) {
        (0, Some(t)) => emit(title(TitleKind::Both, t)),
        (1, Some(t)) => emit(title(TitleKind::Icon, t)),
        (2, Some(t)) => emit(title(TitleKind::Window, t)),
        (7, Some(uri)) => {
            if let Some(cwd) = parse_file_uri(uri) {
                emit(ShellEvent::Cwd(cwd));
            }
        }
        (9, Some(body)) => parse_osc9(body, emit),
        (133, Some(mark)) => parse_mark(mark, false, emit),
        (633, Some(mark)) => parse_mark(mark, true, emit),
        (777, Some(rest)) => parse_osc777(rest, emit),
        (HALYARD_OSC, Some(fields)) => {
            if let Some(report) = parse_report(fields) {
                emit(ShellEvent::Agent(report));
            }
        }
        _ => {}
    }
}

fn title(kind: TitleKind, text: &[u8]) -> ShellEvent {
    ShellEvent::Title { kind, text: sanitize_bytes(text, limits::TITLE) }
}

/// Parses `file://host/path` (or kitty's `kitty-shell-cwd://host/path`).
fn parse_file_uri(uri: &[u8]) -> Option<ReportedCwd> {
    let lower_starts = |prefix: &[u8]| {
        uri.len() >= prefix.len() && uri[..prefix.len()].eq_ignore_ascii_case(prefix)
    };
    let rest = if lower_starts(b"file://") {
        &uri[7..]
    } else if lower_starts(b"kitty-shell-cwd://") {
        &uri[18..]
    } else {
        return None;
    };
    let slash = rest.iter().position(|&b| b == b'/')?;
    let (host, host_exact) = decode_exact(&percent_decode(&rest[..slash]), limits::HOST);
    let (path, path_exact) = decode_exact(&percent_decode(&rest[slash..]), limits::PATH);
    if !path.starts_with('/') {
        return None;
    }
    Some(ReportedCwd {
        host: (!host.is_empty()).then_some(host),
        path,
        verbatim: host_exact && path_exact,
    })
}

/// Sanitizes decoded bytes and reports whether the result is byte for byte the input.
fn decode_exact(bytes: &[u8], cap: usize) -> (String, bool) {
    let clean = sanitize_bytes(bytes, cap);
    let exact = clean.as_bytes() == bytes;
    (clean, exact)
}

fn parse_osc9(body: &[u8], emit: &mut dyn FnMut(ShellEvent)) {
    // ConEmu numeric subcommands: `9;N` or `9;N;...` with N in 1..=12.
    let (head, tail) = split_first(body);
    if let Some(n) = parse_number(head) {
        if n == 4 {
            if let Some(ev) = parse_progress(tail.unwrap_or_default()) {
                emit(ev);
            }
            return;
        }
        if (1..=12).contains(&n) {
            return;
        }
    }
    let text = sanitize_bytes(body, limits::NOTIFICATION);
    if !text.is_empty() {
        emit(ShellEvent::Notification {
            source: NotificationSource::Osc9,
            title: None,
            body: text,
        });
    }
}

fn parse_progress(args: &[u8]) -> Option<ShellEvent> {
    let (st, rest) = split_first(args);
    let state = match parse_number(st)? {
        0 => ProgressState::Remove,
        1 => ProgressState::Normal,
        2 => ProgressState::Error,
        3 => ProgressState::Indeterminate,
        4 => ProgressState::Paused,
        _ => return None,
    };
    let percent = rest.map(|r| split_first(r).0).and_then(parse_number).map(|p| p.min(100) as u8);
    Some(ShellEvent::Progress { state, percent })
}

fn parse_osc777(rest: &[u8], emit: &mut dyn FnMut(ShellEvent)) {
    let (sub, args) = split_first(rest);
    match sub {
        b"notify" => {
            let (title, body) = split_first(args.unwrap_or_default());
            let title = sanitize_bytes(title, limits::NOTIFICATION);
            let body = sanitize_bytes(body.unwrap_or_default(), limits::NOTIFICATION);
            if !title.is_empty() || !body.is_empty() {
                emit(ShellEvent::Notification {
                    source: NotificationSource::Osc777,
                    title: (!title.is_empty()).then_some(title),
                    body,
                });
            }
        }
        b"halyard" => {
            if let Some(report) = parse_report(args.unwrap_or_default()) {
                emit(ShellEvent::Agent(report));
            }
        }
        _ => {}
    }
}

/// Parses `key=value;key=value` fields of the Halyard protocol.
fn parse_report(fields: &[u8]) -> Option<AgentReport> {
    let mut report = AgentReport::default();
    let mut any = false;
    for field in fields.split(|&b| b == b';') {
        let Some(eq) = field.iter().position(|&b| b == b'=') else {
            continue;
        };
        let (key, value) = (&field[..eq], percent_decode(&field[eq + 1..]));
        match key {
            b"state" => {
                if let Some(s) = ReportedState::parse(&String::from_utf8_lossy(&value)) {
                    report.state = Some(s);
                    any = true;
                }
            }
            b"agent" => {
                let v = sanitize_bytes(&value, limits::AGENT_NAME);
                if !v.is_empty() {
                    report.agent = Some(v);
                    any = true;
                }
            }
            b"msg" => {
                let v = sanitize_bytes(&value, limits::AGENT_MESSAGE);
                if !v.is_empty() {
                    report.msg = Some(v);
                    any = true;
                }
            }
            b"session" => {
                let v = sanitize_bytes(&value, limits::SESSION_ID);
                if !v.is_empty() {
                    report.session = Some(v);
                    any = true;
                }
            }
            b"exit" => {
                if let Some(code) = parse_i32(&value) {
                    report.exit_code = Some(code);
                    any = true;
                }
            }
            _ => {}
        }
    }
    any.then_some(report)
}

/// Parses the part of an OSC 133 or 633 mark after the number.
fn parse_mark(mark: &[u8], vscode: bool, emit: &mut dyn FnMut(ShellEvent)) {
    let (kind, args) = split_first(mark);
    let args = args.unwrap_or_default();
    match kind {
        b"A" => emit(ShellEvent::PromptStart { aid: option(args, b"aid", limits::AID) }),
        b"B" => emit(ShellEvent::CommandInputStart),
        b"C" => {
            if !vscode && let Some(url) = raw_option(args, b"cmdline_url") {
                let line = sanitize_bytes(&percent_decode(url), limits::COMMAND_LINE);
                if !line.is_empty() {
                    emit(ShellEvent::CommandLine(line));
                }
            }
            emit(ShellEvent::CommandExecuted);
        }
        b"D" => {
            let (first, _) = split_first(args);
            let exit_code = if first.contains(&b'=') { None } else { parse_i32(first) };
            emit(ShellEvent::CommandFinished { exit_code, aid: option(args, b"aid", limits::AID) });
        }
        b"E" if vscode => {
            let (line, _nonce) = split_first(args);
            let line = sanitize_bytes(&vscode_unescape(line), limits::COMMAND_LINE);
            emit(ShellEvent::CommandLine(line));
        }
        b"P" if vscode => {
            if let Some(cwd) = raw_option(args, b"Cwd") {
                let (path, verbatim) = decode_exact(&vscode_unescape(cwd), limits::PATH);
                if path.starts_with('/') {
                    emit(ShellEvent::Cwd(ReportedCwd { host: None, path, verbatim }));
                }
            }
        }
        _ => {}
    }
}

/// Finds `key=value` among `;` separated options and returns the raw value.
fn raw_option<'a>(args: &'a [u8], key: &[u8]) -> Option<&'a [u8]> {
    args.split(|&b| b == b';').find_map(|opt| {
        let eq = opt.iter().position(|&b| b == b'=')?;
        (&opt[..eq] == key).then(|| &opt[eq + 1..])
    })
}

fn option(args: &[u8], key: &[u8], cap: usize) -> Option<String> {
    let v = sanitize_bytes(raw_option(args, key)?, cap);
    (!v.is_empty()).then_some(v)
}

/// Reverses VS Code's OSC 633 value escaping: `\\` is a backslash and `\xHH` is a
/// byte. Any other backslash is kept literally.
fn vscode_unescape(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        if input[i] == b'\\' {
            if input.get(i + 1) == Some(&b'\\') {
                out.push(b'\\');
                i += 2;
                continue;
            }
            if input.get(i + 1) == Some(&b'x')
                && let (Some(h), Some(l)) = (
                    input.get(i + 2).copied().and_then(hex_val),
                    input.get(i + 3).copied().and_then(hex_val),
                )
            {
                out.push(h << 4 | l);
                i += 4;
                continue;
            }
        }
        out.push(input[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests;
