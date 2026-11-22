//! Saving and restoring workspaces.
//!
//! The file holds layout only: for each window its tabs, their names, the
//! split tree with ratios, which pane had focus, and each pane's working
//! directory. Never scrollback, environment, command history or titles.
//!
//! It lives in the platform state directory (`workspace.json`), written
//! atomically with mode 0600 in a directory with mode 0700. Reading never
//! fails: a missing file means "start fresh", and a damaged one gives the
//! default layout plus a warning.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::tree::{Axis, Node, PaneId, RATIO_LIMITS};
use crate::workspace::{Tab, Workspace, clean_name};

/// File format version written by this build.
pub const VERSION: u32 = 1;
/// File name inside the state directory.
pub const FILE_NAME: &str = "workspace.json";
/// Larger files are not read.
pub const MAX_FILE_BYTES: u64 = 1024 * 1024;
/// Limits applied when restoring, so a hostile or damaged file cannot open
/// thousands of shells.
pub const MAX_WINDOWS: usize = 16;
/// Most tabs restored per window.
pub const MAX_TABS: usize = 64;
/// Most panes restored per tab.
pub const MAX_PANES_PER_TAB: usize = 32;
/// Deepest split nesting restored.
pub const MAX_DEPTH: usize = 16;
/// Most panes restored from one file, across all windows: each pane starts a
/// shell, so a damaged or hostile file must not start thousands.
pub const MAX_PANES_TOTAL: usize = 256;
/// Longest working directory kept, in bytes.
pub const MAX_PATH_BYTES: usize = 4096;

/// The whole saved state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedState {
    /// Format version.
    pub version: u32,
    /// Open windows, in order.
    pub windows: Vec<SavedWindow>,
}

/// One window.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedWindow {
    /// Tabs, in order.
    pub tabs: Vec<SavedTab>,
    /// Index of the active tab.
    #[serde(default)]
    pub active: usize,
}

/// One tab.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedTab {
    /// User-given name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The split tree.
    pub layout: SavedNode,
    /// Index (tree order) of the focused pane.
    #[serde(default)]
    pub focused: usize,
    /// Focus mode.
    #[serde(default)]
    pub zoomed: bool,
}

/// A node of the split tree.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SavedNode {
    /// A pane and the directory its shell was in.
    Pane {
        /// Absolute local path, if known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
    },
    /// A split.
    Split {
        /// Direction.
        axis: Axis,
        /// Share of `first`.
        ratio: f32,
        /// Left or top.
        first: Box<SavedNode>,
        /// Right or bottom.
        second: Box<SavedNode>,
    },
}

/// A restored window: the model, plus each pane's saved directory.
#[derive(Clone, Debug, PartialEq)]
pub struct Restored {
    /// The layout.
    pub workspace: Workspace,
    /// Where each pane's shell should start.
    pub cwds: Vec<(PaneId, Option<PathBuf>)>,
}

/// Result of reading the state file.
#[derive(Clone, Debug, PartialEq)]
pub struct LoadResult {
    /// The windows to open. Empty means "open one default window".
    pub windows: Vec<Restored>,
    /// Problems found, phrased for the user. Never holds a path or name from
    /// the file.
    pub warnings: Vec<String>,
}

/// Validate a working directory for saving or restoring: absolute, no
/// control characters, not too long. Returned unchanged (never resolved or
/// normalized).
#[must_use]
pub fn valid_cwd(p: &str) -> Option<&str> {
    (p.starts_with('/') && p.len() <= MAX_PATH_BYTES && !p.chars().any(char::is_control))
        .then_some(p)
}

/// The local directory an OSC 7 report names, or `None` unless the report
/// is for this host. Accepts `file://host/path` (percent-decoded),
/// `kitty-shell-cwd://host/path` (raw), and a bare absolute path. The host
/// must be empty, `localhost`, or this machine's name.
#[must_use]
pub fn osc7_local_path(raw: &str, hostname: Option<&str>) -> Option<String> {
    let (rest, decode) = if let Some(r) = raw.strip_prefix("file://") {
        (r, true)
    } else if let Some(r) = raw.strip_prefix("kitty-shell-cwd://") {
        (r, false)
    } else if raw.starts_with('/') {
        return valid_cwd(raw).map(str::to_owned);
    } else {
        return None;
    };
    let slash = rest.find('/')?;
    let (host, path) = rest.split_at(slash);
    // The full host name must match: `box` and `box.example.com` may be
    // different machines.
    let local = host.is_empty()
        || host.eq_ignore_ascii_case("localhost")
        || hostname.is_some_and(|h| h.eq_ignore_ascii_case(host));
    if !local {
        return None;
    }
    let path = if decode { percent_decode(path)? } else { path.to_owned() };
    valid_cwd(&path).map(str::to_owned)
}

fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The directory a restored pane starts in: the saved one when it is an
/// absolute path to an existing directory, else `home`.
#[must_use]
pub fn resolve_cwd(saved: Option<&Path>, home: &Path) -> PathBuf {
    saved.filter(|p| p.is_absolute() && p.is_dir()).unwrap_or(home).to_path_buf()
}

/// This machine's host name.
#[cfg(unix)]
#[must_use]
pub fn hostname() -> Option<String> {
    let mut buf = [0u8; 256];
    // SAFETY: the buffer is valid for its length; gethostname writes at most
    // that many bytes.
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if rc != 0 {
        return None;
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf8(buf[..end].to_vec()).ok().filter(|s| !s.is_empty())
}

/// Host names are not read on this platform.
#[cfg(not(unix))]
#[must_use]
pub fn hostname() -> Option<String> {
    None
}

/// The state directory, given an environment lookup. `HALYARD_STATE_DIR`
/// wins; then macOS uses `~/Library/Application Support/Halyard`, Linux
/// `$XDG_STATE_HOME/halyard` (when absolute) or `~/.local/state/halyard`.
pub fn state_dir_with(env: impl Fn(&str) -> Option<OsString>, macos: bool) -> Option<PathBuf> {
    let get = |k: &str| env(k).filter(|v| !v.is_empty());
    if let Some(d) = get("HALYARD_STATE_DIR").map(PathBuf::from).filter(|p| p.is_absolute()) {
        return Some(d);
    }
    let home = get("HOME").map(PathBuf::from).filter(|p| p.is_absolute());
    if macos {
        return home.map(|h| h.join("Library").join("Application Support").join("Halyard"));
    }
    if let Some(x) = get("XDG_STATE_HOME").map(PathBuf::from).filter(|p| p.is_absolute()) {
        return Some(x.join("halyard"));
    }
    home.map(|h| h.join(".local").join("state").join("halyard"))
}

/// The state file for this process.
#[must_use]
pub fn state_file() -> Option<PathBuf> {
    state_dir_with(|k| std::env::var_os(k), cfg!(target_os = "macos")).map(|d| d.join(FILE_NAME))
}

impl Workspace {
    /// The saveable form. `cwd_of` gives each pane's working directory;
    /// anything that is not a valid absolute local path is left out.
    #[must_use]
    pub fn to_saved(&self, cwd_of: &dyn Fn(PaneId) -> Option<String>) -> SavedWindow {
        let tabs = self
            .tabs()
            .iter()
            .map(|t| SavedTab {
                name: t.name.clone(),
                layout: save_node(&t.root, cwd_of),
                focused: t.root.panes().iter().position(|p| *p == t.focused).unwrap_or(0),
                zoomed: t.zoomed,
            })
            .collect();
        SavedWindow { tabs, active: self.active_index() }
    }
}

fn save_node(n: &Node, cwd_of: &dyn Fn(PaneId) -> Option<String>) -> SavedNode {
    match n {
        Node::Pane(id) => SavedNode::Pane { cwd: cwd_of(*id).filter(|c| valid_cwd(c).is_some()) },
        Node::Split { axis, ratio, first, second } => SavedNode::Split {
            axis: *axis,
            ratio: *ratio,
            first: Box::new(save_node(first, cwd_of)),
            second: Box::new(save_node(second, cwd_of)),
        },
    }
}

#[derive(Default)]
struct Issues(BTreeSet<&'static str>);

impl Issues {
    fn add(&mut self, s: &'static str) {
        self.0.insert(s);
    }
}

fn node_stats(n: &SavedNode) -> (usize, usize) {
    match n {
        SavedNode::Pane { .. } => (1, 0),
        SavedNode::Split { first, second, .. } => {
            let (a, da) = node_stats(first);
            let (b, db) = node_stats(second);
            (a + b, 1 + da.max(db))
        }
    }
}

fn restore_node(
    n: &SavedNode,
    next: &mut u64,
    cwds: &mut Vec<(PaneId, Option<PathBuf>)>,
    issues: &mut Issues,
) -> Node {
    match n {
        SavedNode::Pane { cwd } => {
            let id = PaneId(*next);
            *next += 1;
            let dir = cwd.as_deref().and_then(|c| {
                let ok = valid_cwd(c);
                if ok.is_none() {
                    issues.add("a saved working directory was not a valid absolute path");
                }
                ok.map(PathBuf::from)
            });
            cwds.push((id, dir));
            Node::Pane(id)
        }
        SavedNode::Split { axis, ratio, first, second } => {
            let r = if ratio.is_finite() {
                ratio.clamp(RATIO_LIMITS.0, RATIO_LIMITS.1)
            } else {
                issues.add("a saved split ratio was not a number");
                0.5
            };
            Node::Split {
                axis: *axis,
                ratio: r,
                first: Box::new(restore_node(first, next, cwds, issues)),
                second: Box::new(restore_node(second, next, cwds, issues)),
            }
        }
    }
}

fn restore_window(w: &SavedWindow, budget: &mut usize, issues: &mut Issues) -> Option<Restored> {
    let mut next = 1;
    let mut cwds = Vec::new();
    let mut tabs = Vec::new();
    if w.tabs.len() > MAX_TABS {
        issues.add("too many tabs in a saved window; extra tabs were dropped");
    }
    for t in w.tabs.iter().take(MAX_TABS) {
        let (panes, depth) = node_stats(&t.layout);
        if panes > MAX_PANES_PER_TAB || depth > MAX_DEPTH {
            issues.add("a saved tab had too many panes and was dropped");
            continue;
        }
        if panes > *budget {
            issues.add("the saved workspace has too many panes in total; extra tabs were dropped");
            continue;
        }
        *budget -= panes;
        let first_id = next;
        let root = restore_node(&t.layout, &mut next, &mut cwds, issues);
        let ids = root.panes();
        let focused = ids.get(t.focused).copied().unwrap_or(PaneId(first_id));
        let name = t.name.as_deref().map(clean_name).filter(|n| !n.is_empty());
        let zoomed = t.zoomed && ids.len() > 1;
        tabs.push(Tab { root, focused, zoomed, name });
    }
    let workspace = Workspace::from_tabs(tabs, w.active)?;
    Some(Restored { workspace, cwds })
}

/// Turn parsed state into windows, applying the limits.
#[must_use]
pub fn restore(state: &SavedState) -> LoadResult {
    let mut issues = Issues::default();
    if state.version != VERSION {
        return LoadResult {
            windows: Vec::new(),
            warnings: vec![format!(
                "saved workspace has format version {}, this build reads {VERSION}; starting with a new window",
                state.version
            )],
        };
    }
    let mut budget = MAX_PANES_TOTAL;
    if state.windows.len() > MAX_WINDOWS {
        issues.add("too many saved windows; extra windows were dropped");
    }
    let windows: Vec<Restored> = state
        .windows
        .iter()
        .take(MAX_WINDOWS)
        .filter_map(|w| restore_window(w, &mut budget, &mut issues))
        .collect();
    LoadResult { windows, warnings: issues.0.into_iter().map(str::to_owned).collect() }
}

/// Parse the state file's text. Bad JSON gives no windows and a warning
/// with the line number (never the text).
#[must_use]
pub fn parse(text: &str) -> LoadResult {
    match serde_json::from_str::<SavedState>(text) {
        Ok(state) => restore(&state),
        Err(e) => LoadResult {
            windows: Vec::new(),
            warnings: vec![format!(
                "saved workspace could not be read (line {}, {}); starting with a new window",
                e.line(),
                match e.classify() {
                    serde_json::error::Category::Io => "read error",
                    serde_json::error::Category::Syntax => "not valid JSON",
                    serde_json::error::Category::Data => "unexpected content",
                    serde_json::error::Category::Eof => "file ends early",
                }
            )],
        },
    }
}

/// Read the state file. Missing means no windows and no warnings.
#[must_use]
pub fn load(path: &Path) -> LoadResult {
    let empty =
        |w: Option<String>| LoadResult { windows: Vec::new(), warnings: w.into_iter().collect() };
    // Check the type before opening: opening a FIFO with no writer blocks.
    match std::fs::metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return empty(None),
        Err(e) => {
            return empty(Some(format!("saved workspace could not be read: {}", e.kind())));
        }
        Ok(m) if !m.is_file() => {
            return empty(Some("saved workspace is not a regular file; ignored".into()));
        }
        Ok(_) => {}
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    // Non-blocking, so a FIFO swapped in after the check still cannot hang.
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(&mut opts, libc::O_NONBLOCK);
    let file = match opts.open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return empty(None),
        Err(e) => {
            return empty(Some(format!("saved workspace could not be opened: {}", e.kind())));
        }
    };
    match file.metadata() {
        Ok(m) if !m.is_file() => {
            return empty(Some("saved workspace is not a regular file; ignored".into()));
        }
        Ok(m) if m.len() > MAX_FILE_BYTES => {
            return empty(Some("saved workspace is larger than 1 MiB; ignored".into()));
        }
        Err(e) => return empty(Some(format!("saved workspace could not be read: {}", e.kind()))),
        Ok(_) => {}
    }
    let mut text = String::new();
    if let Err(e) = file.take(MAX_FILE_BYTES).read_to_string(&mut text) {
        return empty(Some(format!("saved workspace could not be read: {}", e.kind())));
    }
    parse(&text)
}

/// Write the state atomically (temporary file, fsync, rename) with mode
/// 0600 in a directory with mode 0700 (created, or tightened if it exists).
/// A state directory that is a symlink is refused, and the temporary file
/// is created fresh without following symlinks.
///
/// # Errors
/// Any I/O failure, or a symlinked state directory; the previous file is
/// left in place.
pub fn save(path: &Path, state: &SavedState) -> std::io::Result<()> {
    let dir = path.parent().ok_or_else(|| std::io::Error::other("state file has no directory"))?;
    if std::fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(std::io::Error::other("the state directory is a symlink; not writing there"));
    }
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let json = serde_json::to_vec_pretty(state).map_err(std::io::Error::other)?;
    let tmp = dir.join(format!(".{FILE_NAME}.{}.tmp", std::process::id()));
    // A leftover from a crash (or anything planted there) is removed, not
    // followed: removing a symlink removes the link only.
    match std::fs::remove_file(&tmp) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let result = (|| {
        let mut f = opts.open(&tmp)?;
        #[cfg(unix)]
        {
            // An existing temporary file keeps its old mode; force it.
            use std::os::unix::fs::PermissionsExt;
            f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        f.write_all(&json)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        std::fs::remove_file(&tmp).ok();
    }
    result
}
