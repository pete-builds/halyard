//! `config.toml`: types, parsing, validation and location.
//!
//! Every key is documented in `docs/configuration.md`. The embedded
//! `default-config.toml` is a commented template that parses to exactly
//! [`Config::default`].

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::keys::{Action, KeyChord, Keymap, Platform, UNBIND};
use crate::locate::{
    find_header_line, find_key_line, key_at_offset, line_of_offset, section_at_offset,
};
use crate::theme::{Appearance, DEFAULT_DARK, DEFAULT_LIGHT};

/// The commented default configuration, as printed by `halyard config --print-default`.
#[must_use]
pub fn default_config_toml() -> &'static str {
    include_str!("../default-config.toml")
}

/// A configuration problem: which file, key and line, and what is wrong.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigError {
    /// The config file, when known.
    pub path: Option<PathBuf>,
    /// Dotted key the problem is about, when known.
    pub key: Option<String>,
    /// 1-based line, when known.
    pub line: Option<usize>,
    /// What is wrong.
    pub message: String,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.path, self.line) {
            (Some(p), Some(l)) => write!(f, "{}:{l}: ", p.display())?,
            (Some(p), None) => write!(f, "{}: ", p.display())?,
            (None, Some(l)) => write!(f, "line {l}: ")?,
            (None, None) => {}
        }
        if let Some(k) = &self.key {
            write!(f, "key `{k}`: ")?;
        }
        f.write_str(&self.message)
    }
}

impl std::error::Error for ConfigError {}

impl ConfigError {
    fn at(key: &str, line: Option<usize>, message: impl Into<String>) -> Self {
        Self { path: None, key: Some(key.to_string()), line, message: message.into() }
    }

    fn io(path: &Path, message: String) -> Self {
        Self { path: Some(path.to_path_buf()), key: None, line: None, message }
    }
}

/// A non-fatal problem, such as a key Halyard does not know.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigWarning {
    /// Dotted key the warning is about.
    pub key: String,
    /// 1-based line, when it could be found.
    pub line: Option<usize>,
    /// What to tell the user.
    pub message: String,
}

impl fmt::Display for ConfigWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line {
            Some(l) => write!(f, "line {l}: key `{}`: {}", self.key, self.message),
            None => write!(f, "key `{}`: {}", self.key, self.message),
        }
    }
}

/// A parsed configuration plus the warnings found on the way.
#[derive(Clone, Debug, PartialEq)]
pub struct Loaded {
    /// The configuration.
    pub config: Config,
    /// Unknown keys and similar non-fatal findings.
    pub warnings: Vec<ConfigWarning>,
}

// ---------------------------------------------------------------------------
// Sections
// ---------------------------------------------------------------------------

/// `[font]`
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default)]
pub struct FontConfig {
    /// Font family name, or `monospace` for the system monospace font.
    pub family: String,
    /// Size in points.
    pub size: f32,
    /// Line height as a multiple of the font size.
    pub line_height: f32,
    /// Draw bold text in the bright ANSI colors as well as in bold weight.
    pub bold_is_bright: bool,
}

impl Default for FontConfig {
    fn default() -> Self {
        Self { family: "monospace".into(), size: 13.0, line_height: 1.2, bold_is_bright: false }
    }
}

/// How window decorations are drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decorations {
    /// The system title bar and borders.
    #[default]
    Full,
    /// No title bar or borders.
    None,
    /// macOS: content extends under a transparent title bar.
    TransparentTitlebar,
}

/// `[window]`
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default)]
pub struct WindowConfig {
    /// Space between the window edge and the grid, horizontally, in points.
    pub padding_x: f32,
    /// Space between the window edge and the grid, vertically, in points.
    pub padding_y: f32,
    /// Background opacity, `0.0` (clear) to `1.0` (solid).
    pub opacity: f32,
    /// Blur what is behind a translucent window.
    pub blur: bool,
    /// Window decoration style.
    pub decorations: Decorations,
    /// Columns of a new window.
    pub initial_cols: u16,
    /// Rows of a new window.
    pub initial_rows: u16,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            padding_x: 8.0,
            padding_y: 6.0,
            opacity: 1.0,
            blur: false,
            decorations: Decorations::Full,
            initial_cols: 110,
            initial_rows: 32,
        }
    }
}

/// Cursor shape.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorStyle {
    /// Filled block.
    #[default]
    Block,
    /// Vertical bar.
    Bar,
    /// Underline.
    Underline,
}

/// Where a new shell starts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub enum WorkingDirectory {
    /// The user's home directory.
    #[default]
    Home,
    /// The directory of the focused pane (or the launch directory for the first one).
    Inherit,
    /// A fixed directory: absolute, or starting with `~/`.
    Path(PathBuf),
}

impl TryFrom<String> for WorkingDirectory {
    type Error = String;

    fn try_from(s: String) -> Result<Self, String> {
        match s.as_str() {
            "home" => Ok(Self::Home),
            "inherit" => Ok(Self::Inherit),
            p if p.starts_with('/') || p == "~" || p.starts_with("~/") => {
                Ok(Self::Path(PathBuf::from(p)))
            }
            other => Err(format!(
                "`{other}` is not valid: use `home`, `inherit`, or an absolute path (or one starting with `~/`)"
            )),
        }
    }
}

/// A shell program and its arguments.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ShellConfig {
    /// Program to run, as a path or a name looked up on `PATH`.
    pub program: String,
    /// Arguments passed to it.
    #[serde(default)]
    pub args: Vec<String>,
}

/// `[terminal]`
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default)]
pub struct TerminalConfig {
    /// Lines of scrollback kept per pane.
    pub scrollback_lines: u32,
    /// Shell to run. `None` means the user's login shell (`$SHELL`).
    pub shell: Option<ShellConfig>,
    /// Where new shells start.
    pub working_directory: WorkingDirectory,
    /// Cursor shape.
    pub cursor_style: CursorStyle,
    /// Whether the cursor blinks.
    pub cursor_blink: bool,
    /// Copy the selection to the clipboard as soon as it is made.
    pub copy_on_select: bool,
    /// Ask before closing a pane that is running a command.
    pub confirm_close_running: bool,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            scrollback_lines: 10_000,
            shell: None,
            working_directory: WorkingDirectory::Home,
            cursor_style: CursorStyle::Block,
            cursor_blink: true,
            copy_on_select: false,
            confirm_close_running: true,
        }
    }
}

/// `[theme]`: either one `name`, or a `light` and `dark` pair that follows the system appearance.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ThemeSelection {
    /// One theme for every appearance. Exclusive with `light` and `dark`.
    pub name: Option<String>,
    /// Theme used when the system is in light mode.
    pub light: Option<String>,
    /// Theme used when the system is in dark mode.
    pub dark: Option<String>,
}

impl Default for ThemeSelection {
    fn default() -> Self {
        Self { name: None, light: Some(DEFAULT_LIGHT.into()), dark: Some(DEFAULT_DARK.into()) }
    }
}

impl ThemeSelection {
    /// The theme name to use for the given system appearance. `name` wins when set.
    #[must_use]
    pub fn name_for(&self, appearance: Appearance) -> &str {
        if let Some(n) = &self.name {
            return n;
        }
        match appearance {
            Appearance::Dark => self.dark.as_deref().unwrap_or(DEFAULT_DARK),
            Appearance::Light => self.light.as_deref().unwrap_or(DEFAULT_LIGHT),
        }
    }
}

/// Wire format of an AI provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub enum ProviderKind {
    /// Any endpoint that speaks the OpenAI chat completions API.
    #[serde(rename = "openai-compatible")]
    OpenAiCompatible,
    /// Anthropic's Messages API.
    #[serde(rename = "anthropic")]
    Anthropic,
    /// A local Ollama server.
    #[serde(rename = "ollama")]
    Ollama,
}

/// One `[[ai.providers]]` entry. It never holds a key, only the NAME of an environment variable.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ProviderConfig {
    /// Unique label shown in the UI.
    pub name: String,
    /// Wire format.
    pub kind: ProviderKind,
    /// Base URL. Required for `openai-compatible` and `ollama`.
    #[serde(default)]
    pub base_url: Option<String>,
    /// Model identifier sent with requests.
    pub model: String,
    /// NAME of the environment variable that holds the API key.
    #[serde(default)]
    pub api_key_env: Option<String>,
}

/// `[ai]`
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct AiConfig {
    /// Master switch. Off by default; with it off no AI code runs.
    pub enabled: bool,
    /// Configured providers.
    pub providers: Vec<ProviderConfig>,
}

/// `[agents]`
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct AgentsConfig {
    /// Send a desktop notification when an agent starts waiting for input.
    pub notify_on_waiting: bool,
    /// Extra process names to treat as agents, besides the built-in list.
    pub detect: Vec<String>,
}

impl Default for AgentsConfig {
    fn default() -> Self {
        Self { notify_on_waiting: true, detect: Vec::new() }
    }
}

/// The whole configuration.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Config {
    /// `[font]`
    pub font: FontConfig,
    /// `[window]`
    pub window: WindowConfig,
    /// `[terminal]`
    pub terminal: TerminalConfig,
    /// `[theme]`
    pub theme: ThemeSelection,
    /// `[keybindings]`: chord to action name, or `none` to unbind a default.
    pub keybindings: BTreeMap<String, String>,
    /// `[ai]`
    pub ai: AiConfig,
    /// `[agents]`
    pub agents: AgentsConfig,
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

const TOP_KEYS: &[&str] = &["font", "window", "terminal", "theme", "keybindings", "ai", "agents"];
const FONT_KEYS: &[&str] = &["family", "size", "line_height", "bold_is_bright"];
const WINDOW_KEYS: &[&str] =
    &["padding_x", "padding_y", "opacity", "blur", "decorations", "initial_cols", "initial_rows"];
const TERMINAL_KEYS: &[&str] = &[
    "scrollback_lines",
    "shell",
    "working_directory",
    "cursor_style",
    "cursor_blink",
    "copy_on_select",
    "confirm_close_running",
];
const SHELL_KEYS: &[&str] = &["program", "args"];
const THEME_KEYS: &[&str] = &["name", "light", "dark"];
const AI_KEYS: &[&str] = &["enabled", "providers"];
const PROVIDER_KEYS: &[&str] = &["name", "kind", "base_url", "model", "api_key_env"];
const AGENTS_KEYS: &[&str] = &["notify_on_waiting", "detect"];
/// Substrings that make a key name look like it holds a secret (matched case-insensitively).
/// Applies to every key under `[ai]`, except the documented names such as `api_key_env`.
const SECRET_FRAGMENTS: &[&str] = &["key", "token", "secret", "password", "passwd", "auth"];

/// Shown instead of serde's or toml's own message for anything wrong inside `[ai]`, because
/// those messages quote the offending value and a user may have pasted a key there.
const AI_REDACTED: &str = "invalid value or syntax; details withheld because the value of an \
     `[ai]` key could be a secret and is never echoed. See docs/configuration.md for the \
     expected type, and if you pasted a key here, revoke it";

/// Whether `key` is a documented `[ai]` or `[[ai.providers]]` key.
fn is_known_ai_key(key: &str) -> bool {
    AI_KEYS.contains(&key) || PROVIDER_KEYS.contains(&key)
}

/// Whether an undocumented `[ai]` key looks like it holds a secret.
fn is_secret_key(key: &str) -> bool {
    if is_known_ai_key(key) {
        return false;
    }
    let lower = key.to_ascii_lowercase();
    SECRET_FRAGMENTS.iter().any(|f| lower.contains(f))
}

/// Whether the section name is `ai` or inside it.
fn is_ai_section(section: &str) -> bool {
    section == "ai" || section.starts_with("ai.")
}

/// Best-effort line of `key` in `section`, falling back to the table header, then to
/// the key that holds an inline table (`shell = { ... }`).
fn line_for(src: &str, section: &str, occurrence: usize, key: &str) -> Option<usize> {
    find_key_line(src, section, occurrence, key)
        .or_else(|| find_header_line(src, section, occurrence))
        .or_else(|| {
            let (parent, last) = section.rsplit_once('.')?;
            find_key_line(src, parent, 0, last)
        })
}

fn unknown_in(
    out: &mut Vec<ConfigWarning>,
    src: &str,
    table: &toml::Table,
    section: &str,
    known: &[&str],
) {
    for k in table.keys().filter(|k| !known.contains(&k.as_str())) {
        let full = if section.is_empty() { k.clone() } else { format!("{section}.{k}") };
        let line = if section.is_empty() {
            find_key_line(src, "", 0, k).or_else(|| find_header_line(src, k, 0))
        } else {
            line_for(src, section, 0, k)
        };
        let mut message = "unknown key, ignored".to_string();
        if let Some(near) = known.iter().find(|c| near_miss(c, k)) {
            message.push_str(&format!(" (did you mean `{near}`?)"));
        }
        out.push(ConfigWarning { key: full, line, message });
    }
}

/// Cheap typo detector: equal after removing `-` and `_`, or about one edit apart.
fn near_miss(known: &str, given: &str) -> bool {
    let norm = |s: &str| s.replace(['-', '_'], "").to_ascii_lowercase();
    let (a, b) = (norm(known), norm(given));
    if a == b {
        return true;
    }
    let (x, y): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let sorted = |v: &[char]| {
        let mut s = v.to_vec();
        s.sort_unstable();
        s
    };
    if x.len() == y.len() && sorted(&x) == sorted(&y) {
        return true;
    }
    if x.len().abs_diff(y.len()) > 1 {
        return false;
    }
    let head = x.iter().zip(&y).take_while(|(p, q)| p == q).count();
    let tail = x.iter().rev().zip(y.iter().rev()).take_while(|(p, q)| p == q).count();
    head + tail + 1 >= x.len().max(y.len())
}

fn collect_warnings(src: &str, root: &toml::Table) -> Vec<ConfigWarning> {
    let mut w = Vec::new();
    unknown_in(&mut w, src, root, "", TOP_KEYS);
    let sub = |name: &str| root.get(name).and_then(toml::Value::as_table);
    let sections: [(&str, &[&str]); 6] = [
        ("font", FONT_KEYS),
        ("window", WINDOW_KEYS),
        ("terminal", TERMINAL_KEYS),
        ("theme", THEME_KEYS),
        ("ai", AI_KEYS),
        ("agents", AGENTS_KEYS),
    ];
    for (name, known) in sections {
        if let Some(t) = sub(name) {
            unknown_in(&mut w, src, t, name, known);
        }
    }
    if let Some(shell) =
        sub("terminal").and_then(|t| t.get("shell")).and_then(toml::Value::as_table)
    {
        unknown_in(&mut w, src, shell, "terminal.shell", SHELL_KEYS);
    }
    for (i, t) in providers(root).enumerate() {
        for k in t.keys().filter(|k| !PROVIDER_KEYS.contains(&k.as_str()) && !is_secret_key(k)) {
            w.push(ConfigWarning {
                key: format!("ai.providers[{i}].{k}"),
                line: line_for(src, "ai.providers", i, k),
                message: "unknown key, ignored".into(),
            });
        }
    }
    w
}

fn providers(root: &toml::Table) -> impl Iterator<Item = &toml::Table> {
    root.get("ai")
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get("providers"))
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(toml::Value::as_table)
}

fn secret_error(key: &str, line: Option<usize>) -> ConfigError {
    ConfigError::at(
        key,
        line,
        "Halyard never stores API keys in the config file. Export the key in an environment \
         variable (or keep it in your keychain or secret manager and export it from there) \
         and set `api_key_env = \"NAME_OF_VARIABLE\"` instead",
    )
}

/// Reject any secret-looking key anywhere under `[ai]`, including keys nested in tables
/// such as `headers`. Only key NAMES are reported, never values.
fn reject_secrets(src: &str, root: &toml::Table) -> Result<(), ConfigError> {
    let Some(ai) = root.get("ai").and_then(toml::Value::as_table) else {
        return Ok(());
    };
    if let Some(k) = ai.keys().find(|k| is_secret_key(k)) {
        return Err(secret_error(&format!("ai.{k}"), line_for(src, "ai", 0, k)));
    }
    for (i, t) in providers(root).enumerate() {
        if let Some(k) = t.keys().find(|k| is_secret_key(k)) {
            return Err(secret_error(
                &format!("ai.providers[{i}].{k}"),
                line_for(src, "ai.providers", i, k),
            ));
        }
        for (outer, v) in t {
            if let Some(inner) = v.as_table()
                && let Some(k) = inner.keys().find(|k| is_secret_key(k))
            {
                let section = format!("ai.providers.{outer}");
                return Err(secret_error(
                    &format!("ai.providers[{i}].{outer}.{k}"),
                    line_for(src, &section, 0, k),
                ));
            }
        }
    }
    Ok(())
}

/// Whether an `http(s)` URL carries `userinfo@` in its authority.
fn has_userinfo(url: &str) -> bool {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    authority.contains('@')
}

fn valid_env_name(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

impl Config {
    /// Parse and validate a config file's text.
    ///
    /// Unknown keys become warnings; everything else that is wrong is an error
    /// naming the key and line.
    ///
    /// # Errors
    /// Returns a [`ConfigError`] for TOML syntax errors, wrong types, unknown
    /// enum values, out-of-range numbers, bad key chords, a key stored in the
    /// config, and similar.
    pub fn parse(src: &str) -> Result<Loaded, ConfigError> {
        let root: toml::Table = toml::from_str(src).map_err(|e| {
            let in_ai = e.span().is_some_and(|s| is_ai_section(&section_at_offset(src, s.start)));
            ConfigError {
                path: None,
                key: None,
                line: e.span().map(|s| line_of_offset(src, s.start)),
                message: if in_ai { AI_REDACTED.to_string() } else { e.message().to_string() },
            }
        })?;
        reject_secrets(src, &root)?;
        let warnings = collect_warnings(src, &root);
        let config: Config = toml::from_str(src).map_err(|e| {
            let (key, line) = match e.span() {
                Some(s) => key_at_offset(src, s.start),
                None => (None, 1),
            };
            let in_ai = key.as_deref().is_some_and(|k| k == "ai" || k.starts_with("ai."))
                || e.span().is_some_and(|s| is_ai_section(&section_at_offset(src, s.start)));
            let message = if in_ai { AI_REDACTED.to_string() } else { e.message().to_string() };
            ConfigError { path: None, key, line: Some(line), message }
        })?;
        config.validate(src)?;
        Ok(Loaded { config, warnings })
    }

    /// Read and parse a file. A missing file gives the defaults and no warnings.
    ///
    /// # Errors
    /// Returns a [`ConfigError`] (with `path` set) when the file cannot be
    /// read or fails [`Config::parse`].
    pub fn load(path: &Path) -> Result<Loaded, ConfigError> {
        let src = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Loaded { config: Config::default(), warnings: Vec::new() });
            }
            Err(e) => return Err(ConfigError::io(path, format!("cannot read config file: {e}"))),
        };
        Self::parse(&src).map_err(|mut e| {
            e.path = Some(path.to_path_buf());
            e
        })
    }

    fn validate(&self, src: &str) -> Result<(), ConfigError> {
        let range =
            |section: &str, key: &str, v: f64, lo: f64, hi: f64| -> Result<(), ConfigError> {
                if v.is_nan() || v < lo || v > hi {
                    Err(ConfigError::at(
                        &format!("{section}.{key}"),
                        line_for(src, section, 0, key),
                        format!("{v} is out of range; expected {lo} to {hi}"),
                    ))
                } else {
                    Ok(())
                }
            };
        let (f, w, t) = (&self.font, &self.window, &self.terminal);
        if f.family.trim().is_empty() {
            return Err(ConfigError::at(
                "font.family",
                line_for(src, "font", 0, "family"),
                "must not be empty",
            ));
        }
        range("font", "size", f64::from(f.size), 4.0, 128.0)?;
        range("font", "line_height", f64::from(f.line_height), 0.8, 3.0)?;
        range("window", "padding_x", f64::from(w.padding_x), 0.0, 200.0)?;
        range("window", "padding_y", f64::from(w.padding_y), 0.0, 200.0)?;
        range("window", "opacity", f64::from(w.opacity), 0.0, 1.0)?;
        range("window", "initial_cols", f64::from(w.initial_cols), 10.0, 1000.0)?;
        range("window", "initial_rows", f64::from(w.initial_rows), 2.0, 1000.0)?;
        range("terminal", "scrollback_lines", f64::from(t.scrollback_lines), 0.0, 1_000_000.0)?;
        if t.shell.as_ref().is_some_and(|s| s.program.trim().is_empty()) {
            return Err(ConfigError::at(
                "terminal.shell.program",
                line_for(src, "terminal.shell", 0, "program"),
                "must not be empty",
            ));
        }
        self.validate_theme(src)?;
        for (i, p) in self.ai.providers.iter().enumerate() {
            self.validate_provider(src, i, p)?;
        }
        for (i, name) in self.agents.detect.iter().enumerate() {
            if name.trim().is_empty() {
                return Err(ConfigError::at(
                    &format!("agents.detect[{i}]"),
                    line_for(src, "agents", 0, "detect"),
                    "process names must not be empty",
                ));
            }
        }
        self.keymap_checked(Platform::Mac, src)?;
        self.keymap_checked(Platform::Linux, src)?;
        Ok(())
    }

    fn validate_theme(&self, src: &str) -> Result<(), ConfigError> {
        // `light` and `dark` default to the bundled pair, so a conflict can only
        // be seen from what the user actually wrote.
        let written = |k: &str| find_key_line(src, "theme", 0, k);
        if self.theme.name.is_some()
            && let Some(l) = written("light").or_else(|| written("dark"))
        {
            return Err(ConfigError::at(
                "theme.name",
                written("name"),
                format!(
                    "cannot be combined with `light` or `dark` (line {l}); use `name` alone, or `light` and `dark`"
                ),
            ));
        }
        Ok(())
    }

    fn validate_provider(
        &self,
        src: &str,
        i: usize,
        p: &ProviderConfig,
    ) -> Result<(), ConfigError> {
        let l = |k: &str| line_for(src, "ai.providers", i, k);
        let key = |k: &str| format!("ai.providers[{i}].{k}");
        if p.name.trim().is_empty() {
            return Err(ConfigError::at(&key("name"), l("name"), "must not be empty"));
        }
        if self.ai.providers[..i].iter().any(|q| q.name == p.name) {
            return Err(ConfigError::at(
                &key("name"),
                l("name"),
                "duplicate provider name (the name is not shown)",
            ));
        }
        if p.model.trim().is_empty() {
            return Err(ConfigError::at(&key("model"), l("model"), "must not be empty"));
        }
        match (&p.base_url, p.kind) {
            (None, ProviderKind::OpenAiCompatible | ProviderKind::Ollama) => {
                return Err(ConfigError::at(
                    &key("base_url"),
                    l("kind"),
                    "required for this provider kind",
                ));
            }
            (Some(u), _) if !(u.starts_with("http://") || u.starts_with("https://")) => {
                return Err(ConfigError::at(
                    &key("base_url"),
                    l("base_url"),
                    "must start with http:// or https:// (the value is not shown; if you pasted \
                     a key here, revoke it)",
                ));
            }
            (Some(u), _) if has_userinfo(u) => {
                return Err(ConfigError::at(
                    &key("base_url"),
                    l("base_url"),
                    "must not contain credentials (`user:password@host`); the value is not \
                     shown. Put the key in an environment variable and name it with \
                     `api_key_env`; if you pasted a key here, revoke it",
                ));
            }
            _ => {}
        }
        if let Some(env) = &p.api_key_env
            && !valid_env_name(env)
        {
            return Err(ConfigError::at(
                &key("api_key_env"),
                l("api_key_env"),
                "must be the NAME of an environment variable (letters, digits, underscore), not a key; \
                     if you pasted a key here, revoke it",
            ));
        }
        Ok(())
    }

    fn keymap_checked(&self, platform: Platform, src: &str) -> Result<Keymap, ConfigError> {
        let mut map = Keymap::defaults(platform);
        let mut seen: BTreeMap<KeyChord, &str> = BTreeMap::new();
        for (chord_s, action_s) in &self.keybindings {
            let key = format!("keybindings.{chord_s}");
            let line = find_key_line(src, "keybindings", 0, chord_s);
            let chord = KeyChord::parse(chord_s, platform)
                .map_err(|e| ConfigError::at(&key, line, e.to_string()))?;
            if let Some(prev) = seen.insert(chord, chord_s) {
                return Err(ConfigError::at(
                    &key,
                    line,
                    format!("`{chord_s}` and `{prev}` are the same chord on this platform"),
                ));
            }
            let action = if action_s == UNBIND {
                None
            } else {
                Some(Action::from_name(action_s).ok_or_else(|| {
                    let names: Vec<&str> = Action::ALL.iter().map(|a| a.name()).collect();
                    ConfigError::at(
                        &key,
                        line,
                        format!(
                            "`{action_s}` is not an action; use `none` or one of: {}",
                            names.join(", ")
                        ),
                    )
                })?)
            };
            map.set(chord, action);
        }
        Ok(map)
    }

    /// The effective keymap for a platform: the defaults with the user's
    /// `[keybindings]` applied on top.
    ///
    /// # Errors
    /// A bad chord or action name. [`Config::parse`] already rejects these, so
    /// this only fails for a `Config` built by hand.
    pub fn keymap(&self, platform: Platform) -> Result<Keymap, ConfigError> {
        self.keymap_checked(platform, "")
    }
}

// ---------------------------------------------------------------------------
// Location
// ---------------------------------------------------------------------------

/// Where the config file lives, given an environment lookup. Testable core of [`config_path`].
///
/// Order: `$HALYARD_CONFIG`, then `$XDG_CONFIG_HOME/halyard/config.toml`, then
/// `~/.config/halyard/config.toml`. The last form is used on macOS too, not
/// `~/Library/Application Support`, so dotfiles port between machines.
/// Empty variables are ignored, and so is a relative `XDG_CONFIG_HOME`, as the XDG spec says.
pub fn config_path_with(env: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let get = |k: &str| env(k).filter(|v| !v.is_empty());
    if let Some(p) = get("HALYARD_CONFIG") {
        return Some(PathBuf::from(p));
    }
    if let Some(x) = get("XDG_CONFIG_HOME").map(PathBuf::from).filter(|p| p.is_absolute()) {
        return Some(x.join("halyard").join("config.toml"));
    }
    get("HOME").map(|h| PathBuf::from(h).join(".config").join("halyard").join("config.toml"))
}

/// Where the config file lives for this process. See [`config_path_with`].
#[must_use]
pub fn config_path() -> Option<PathBuf> {
    config_path_with(|k| std::env::var_os(k))
}

/// The user themes directory: `themes/` next to the config file.
#[must_use]
pub fn themes_dir_for(config_path: &Path) -> PathBuf {
    config_path.parent().map_or_else(|| PathBuf::from("themes"), |p| p.join("themes"))
}
