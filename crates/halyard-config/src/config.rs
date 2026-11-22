//! `config.toml`: types, parsing, validation and location.
//!
//! Every key is documented in `docs/configuration.md`. The embedded
//! `default-config.toml` is a commented template that parses to exactly
//! [`Config::default`].

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use toml::de::{DeTable, DeValue};

use crate::fsread::{Refusal, read_regular};
use crate::keys::{Action, KeyChord, Keymap, Platform, UNBIND};
use crate::locate::{Index, line_of_offset, parse_doc, value_free};
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
            _ => Err(
                "not valid: use `home`, `inherit`, or an absolute path (or one starting with `~/`)"
                    .to_string(),
            ),
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
    /// Ask before pasting text with a line break while the program has not
    /// turned on bracketed paste (so the shell would run each line).
    pub confirm_multiline_paste: bool,
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
            confirm_multiline_paste: true,
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

/// What a key's value must be. The checks run on the parsed document, so no error needs to
/// quote the value to say what was wrong.
#[derive(Clone, Copy)]
enum Kind {
    Bool,
    Number,
    Int(i128, i128),
    Str,
    StrArray,
    Enum(&'static [&'static str]),
    /// Checked by dedicated code.
    Custom,
}

impl Kind {
    fn describe(self) -> String {
        match self {
            Kind::Bool => "a boolean (true or false)".into(),
            Kind::Number => "a number".into(),
            Kind::Int(lo, hi) => format!("an integer from {lo} to {hi}"),
            Kind::Str => "a string".into(),
            Kind::StrArray => "an array of strings".into(),
            Kind::Enum(names) => {
                let list: Vec<String> = names.iter().map(|n| format!("`{n}`")).collect();
                format!("one of {}", list.join(", "))
            }
            Kind::Custom => "a valid value".into(),
        }
    }

    fn accepts(self, v: &DeValue<'_>) -> bool {
        match (self, v) {
            (Kind::Bool, DeValue::Boolean(_))
            | (Kind::Number, DeValue::Integer(_) | DeValue::Float(_))
            | (Kind::Str, DeValue::String(_))
            | (Kind::Custom, _) => true,
            (Kind::Int(lo, hi), DeValue::Integer(i)) => {
                i128::from_str_radix(i.as_str(), i.radix()).is_ok_and(|n| (lo..=hi).contains(&n))
            }
            (Kind::StrArray, DeValue::Array(a)) => {
                a.iter().all(|x| matches!(x.get_ref(), DeValue::String(_)))
            }
            (Kind::Enum(names), DeValue::String(s)) => names.contains(&s.as_ref()),
            _ => false,
        }
    }
}

type Schema = &'static [(&'static str, Kind)];

const FONT: Schema = &[
    ("family", Kind::Str),
    ("size", Kind::Number),
    ("line_height", Kind::Number),
    ("bold_is_bright", Kind::Bool),
];
const WINDOW: Schema = &[
    ("padding_x", Kind::Number),
    ("padding_y", Kind::Number),
    ("opacity", Kind::Number),
    ("blur", Kind::Bool),
    ("decorations", Kind::Enum(&["full", "none", "transparent_titlebar"])),
    ("initial_cols", Kind::Int(0, 65_535)),
    ("initial_rows", Kind::Int(0, 65_535)),
];
const TERMINAL: Schema = &[
    ("scrollback_lines", Kind::Int(0, 4_294_967_295)),
    ("shell", Kind::Custom),
    ("working_directory", Kind::Str),
    ("cursor_style", Kind::Enum(&["block", "bar", "underline"])),
    ("cursor_blink", Kind::Bool),
    ("copy_on_select", Kind::Bool),
    ("confirm_close_running", Kind::Bool),
    ("confirm_multiline_paste", Kind::Bool),
];
const SHELL: Schema = &[("program", Kind::Str), ("args", Kind::StrArray)];
const THEME: Schema = &[("name", Kind::Str), ("light", Kind::Str), ("dark", Kind::Str)];
const AI: Schema = &[("enabled", Kind::Bool), ("providers", Kind::Custom)];
const PROVIDER: Schema = &[
    ("name", Kind::Str),
    ("kind", Kind::Enum(&["openai-compatible", "anthropic", "ollama"])),
    ("base_url", Kind::Str),
    ("model", Kind::Str),
    ("api_key_env", Kind::Str),
];
const AGENTS: Schema = &[("notify_on_waiting", Kind::Bool), ("detect", Kind::StrArray)];

/// Substrings that make a key name look like it holds a secret (matched case-insensitively).
/// Applies to every key under `[ai]`, except the documented names such as `api_key_env`.
const SECRET_FRAGMENTS: &[&str] = &["key", "token", "secret", "password", "passwd", "auth"];

fn in_schema(schema: Schema, key: &str) -> bool {
    schema.iter().any(|(n, _)| *n == key)
}

/// Whether an undocumented `[ai]` key looks like it holds a secret.
fn is_secret_key(key: &str) -> bool {
    if in_schema(AI, key) || in_schema(PROVIDER, key) {
        return false;
    }
    let lower = key.to_ascii_lowercase();
    SECRET_FRAGMENTS.iter().any(|f| lower.contains(f))
}

type Spanned<T> = toml::Spanned<T>;

fn entry<'a, 'i>(t: &'a DeTable<'i>, name: &str) -> Option<&'a Spanned<DeValue<'i>>> {
    t.iter().find(|(k, _)| k.get_ref().as_ref() == name).map(|(_, v)| v)
}

fn sub_table<'a, 'i>(t: &'a DeTable<'i>, name: &str) -> Option<&'a DeTable<'i>> {
    entry(t, name).and_then(|v| v.get_ref().as_table())
}

fn key_line(src: &str, t: &DeTable<'_>, name: &str) -> Option<usize> {
    t.iter()
        .find(|(k, _)| k.get_ref().as_ref() == name)
        .map(|(k, _)| line_of_offset(src, k.span().start))
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

/// Unknown keys in a section become warnings. Under `[ai]` the name is NOT shown, because an
/// unknown name there may be a pasted secret.
fn unknown_in(
    out: &mut Vec<ConfigWarning>,
    src: &str,
    table: &DeTable<'_>,
    section: &str,
    schema: Option<Schema>,
    known_top: &[&str],
    hide_names: bool,
) {
    for (k, _) in table {
        let name = k.get_ref().as_ref();
        let known = schema.map_or_else(|| known_top.contains(&name), |s| in_schema(s, name));
        if known {
            continue;
        }
        let line = Some(line_of_offset(src, k.span().start));
        if hide_names {
            let place = if section.contains("providers") { "[[ai.providers]]" } else { "[ai]" };
            out.push(ConfigWarning {
                key: section.to_string(),
                line,
                message: format!("unknown key in {place}, ignored (the name is not shown)"),
            });
            continue;
        }
        let full = if section.is_empty() { name.to_string() } else { format!("{section}.{name}") };
        let mut message = "unknown key, ignored".to_string();
        let names: Vec<&str> =
            schema.map_or_else(|| known_top.to_vec(), |s| s.iter().map(|(n, _)| *n).collect());
        if let Some(near) = names.iter().find(|c| near_miss(c, name)) {
            message.push_str(&format!(" (did you mean `{near}`?)"));
        }
        out.push(ConfigWarning { key: full, line, message });
    }
}

fn collect_warnings(src: &str, root: &DeTable<'_>) -> Vec<ConfigWarning> {
    let mut w = Vec::new();
    unknown_in(&mut w, src, root, "", None, TOP_KEYS, false);
    for (name, schema) in [
        ("font", FONT),
        ("window", WINDOW),
        ("terminal", TERMINAL),
        ("theme", THEME),
        ("agents", AGENTS),
    ] {
        if let Some(t) = sub_table(root, name) {
            unknown_in(&mut w, src, t, name, Some(schema), &[], false);
        }
    }
    if let Some(shell) = sub_table(root, "terminal").and_then(|t| sub_table(t, "shell")) {
        unknown_in(&mut w, src, shell, "terminal.shell", Some(SHELL), &[], false);
    }
    if let Some(ai) = sub_table(root, "ai") {
        unknown_in(&mut w, src, ai, "ai", Some(AI), &[], true);
        for (i, p) in provider_tables(ai).enumerate() {
            unknown_in(&mut w, src, p, &format!("ai.providers[{i}]"), Some(PROVIDER), &[], true);
        }
    }
    w
}

fn provider_tables<'a, 'i>(ai: &'a DeTable<'i>) -> impl Iterator<Item = &'a DeTable<'i>> {
    entry(ai, "providers")
        .and_then(|v| v.get_ref().as_array())
        .into_iter()
        .flat_map(|a| a.iter())
        .filter_map(|v| v.get_ref().as_table())
}

fn secret_error(section: &str, line: Option<usize>) -> ConfigError {
    ConfigError::at(
        section,
        line,
        "a key here is named like a secret (key, token, secret, password or auth; the name is \
         not shown). Halyard never stores API keys in the config file. Export the key in an \
         environment variable (or keep it in your keychain or secret manager and export it \
         from there) and set `api_key_env = \"NAME_OF_VARIABLE\"` instead",
    )
}

/// Find a secret-looking key name anywhere under `table`, however deeply nested.
fn find_secret_name(src: &str, table: &DeTable<'_>) -> Option<usize> {
    for (k, v) in table {
        if is_secret_key(k.get_ref().as_ref()) {
            return Some(line_of_offset(src, k.span().start));
        }
        let nested = match v.get_ref() {
            DeValue::Table(t) => find_secret_name(src, t),
            DeValue::Array(a) => a
                .iter()
                .filter_map(|x| x.get_ref().as_table())
                .find_map(|t| find_secret_name(src, t)),
            _ => None,
        };
        if nested.is_some() {
            return nested;
        }
    }
    None
}

/// A secret-looking key NAME outside `[ai]`: top level, any section, any depth. Documented
/// names (such as the `keybindings` section) are exempt, and the chords inside `[keybindings]`
/// are not checked at all. Returns the dotted path and line. Only the name is ever shown.
fn find_secret_outside_ai(
    src: &str,
    table: &DeTable<'_>,
    prefix: &str,
    top: bool,
) -> Option<(String, usize)> {
    const SECTION_SCHEMAS: [Schema; 6] = [FONT, WINDOW, TERMINAL, SHELL, THEME, AGENTS];
    for (k, v) in table {
        let name = k.get_ref().as_ref();
        let path = if prefix.is_empty() { name.to_string() } else { format!("{prefix}.{name}") };
        if top && matches!(name, "ai" | "keybindings") {
            continue;
        }
        let known =
            (top && TOP_KEYS.contains(&name)) || SECTION_SCHEMAS.iter().any(|s| in_schema(s, name));
        let lower = name.to_ascii_lowercase();
        if !known && SECRET_FRAGMENTS.iter().any(|f| lower.contains(f)) {
            return Some((path, line_of_offset(src, k.span().start)));
        }
        let nested = match v.get_ref() {
            DeValue::Table(t) => find_secret_outside_ai(src, t, &path, false),
            DeValue::Array(a) => a
                .iter()
                .filter_map(|x| x.get_ref().as_table())
                .find_map(|t| find_secret_outside_ai(src, t, &path, false)),
            _ => None,
        };
        if nested.is_some() {
            return nested;
        }
    }
    None
}

/// Reject any secret-looking key name anywhere in the file. Decided from the parsed tree, so
/// quoting, dotted keys and multi-line strings cannot hide one. Under `[ai]` the name is never
/// reported; elsewhere the key path (a name) is, and the value never is.
fn reject_secrets(src: &str, root: &DeTable<'_>) -> Result<(), ConfigError> {
    if let Some((path, line)) = find_secret_outside_ai(src, root, "", true) {
        return Err(ConfigError::at(
            &path,
            Some(line),
            "this key is named like a secret (key, token, secret, password or auth). Halyard never \
             stores API keys in the config file. Export the key in an environment variable (or keep \
             it in your keychain or secret manager and export it from there) and set \
             `api_key_env = \"NAME_OF_VARIABLE\"` on an `[[ai.providers]]` entry instead",
        ));
    }
    let Some(ai) = sub_table(root, "ai") else { return Ok(()) };
    if let Some(line) = find_secret_name(src, ai) {
        // Say where: the nearest documented section, never the name.
        let providers: Vec<&DeTable<'_>> = provider_tables(ai).collect();
        let section = providers
            .iter()
            .position(|p| find_secret_name(src, p).is_some())
            .map_or_else(|| "ai".to_string(), |i| format!("ai.providers[{i}]"));
        return Err(secret_error(&section, Some(line)));
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

fn type_error(path: &str, line: Option<usize>, kind: Kind) -> ConfigError {
    ConfigError::at(path, line, format!("expected {}", kind.describe()))
}

/// Check every value's type, enum membership and integer range against the schema.
/// The messages state what was expected, never what was found.
fn check_section(
    src: &str,
    path: &str,
    table: &DeTable<'_>,
    schema: Schema,
) -> Result<(), ConfigError> {
    for (k, v) in table {
        let name = k.get_ref().as_ref();
        if let Some((_, kind)) = schema.iter().find(|(n, _)| *n == name)
            && !kind.accepts(v.get_ref())
        {
            return Err(type_error(
                &format!("{path}.{name}"),
                Some(line_of_offset(src, k.span().start)),
                *kind,
            ));
        }
    }
    Ok(())
}

fn require_keys(
    path: &str,
    table: &DeTable<'_>,
    parent_line: Option<usize>,
    names: &[&str],
) -> Result<(), ConfigError> {
    for n in names {
        if entry(table, n).is_none() {
            return Err(ConfigError::at(path, parent_line, format!("missing required key `{n}`")));
        }
    }
    Ok(())
}

fn check_types(src: &str, root: &DeTable<'_>) -> Result<(), ConfigError> {
    for name in TOP_KEYS {
        if let Some(v) = entry(root, name)
            && !matches!(v.get_ref(), DeValue::Table(_))
        {
            return Err(ConfigError::at(
                name,
                key_line(src, root, name),
                "expected a table (a section)",
            ));
        }
    }
    for (name, schema) in [
        ("font", FONT),
        ("window", WINDOW),
        ("terminal", TERMINAL),
        ("theme", THEME),
        ("agents", AGENTS),
        ("ai", AI),
    ] {
        if let Some(t) = sub_table(root, name) {
            check_section(src, name, t, schema)?;
        }
    }
    if let Some(t) = sub_table(root, "terminal") {
        if let Some(wd) = entry(t, "working_directory").and_then(|v| match v.get_ref() {
            DeValue::String(s) => Some(s.as_ref().to_string()),
            _ => None,
        }) && !(matches!(wd.as_str(), "home" | "inherit" | "~")
            || wd.starts_with('/')
            || wd.starts_with("~/"))
        {
            return Err(ConfigError::at(
                "terminal.working_directory",
                key_line(src, t, "working_directory"),
                "must be `home`, `inherit`, or an absolute path (or one starting with `~/`)",
            ));
        }
        if let Some(shell) = entry(t, "shell") {
            let line = key_line(src, t, "shell");
            let DeValue::Table(st) = shell.get_ref() else {
                return Err(ConfigError::at(
                    "terminal.shell",
                    line,
                    "expected a table with `program` and optional `args`",
                ));
            };
            check_section(src, "terminal.shell", st, SHELL)?;
            require_keys("terminal.shell", st, line, &["program"])?;
        }
    }
    if let Some(kb) = entry(root, "keybindings").and_then(|v| v.get_ref().as_table()) {
        for (k, v) in kb {
            if !matches!(v.get_ref(), DeValue::String(_)) {
                return Err(type_error(
                    &format!("keybindings.{}", k.get_ref()),
                    Some(line_of_offset(src, k.span().start)),
                    Kind::Str,
                ));
            }
        }
    }
    if let Some(ai) = sub_table(root, "ai")
        && let Some(p) = entry(ai, "providers")
    {
        let line = key_line(src, ai, "providers");
        let DeValue::Array(items) = p.get_ref() else {
            return Err(ConfigError::at(
                "ai.providers",
                line,
                "expected an array of tables (`[[ai.providers]]`)",
            ));
        };
        for (i, item) in items.iter().enumerate() {
            let path = format!("ai.providers[{i}]");
            let item_line = Some(line_of_offset(src, item.span().start));
            let DeValue::Table(t) = item.get_ref() else {
                return Err(ConfigError::at(&path, item_line, "expected a table"));
            };
            check_section(src, &path, t, PROVIDER)?;
            require_keys(&path, t, item_line, &["name", "kind", "model"])?;
        }
    }
    Ok(())
}

impl Config {
    /// Parse and validate a config file's text.
    ///
    /// Unknown keys become warnings; everything else that is wrong is an error
    /// naming the key and line. No error or warning ever contains a VALUE from the
    /// file, because any value might be a pasted secret. Messages hold key paths, line
    /// numbers and what was expected. Under `[ai]` they do not hold unknown key names
    /// either. See "Error messages" in `docs/configuration.md`.
    ///
    /// # Errors
    /// Returns a [`ConfigError`] for TOML syntax errors, wrong types, unknown
    /// enum values, out-of-range numbers, bad key chords, a key stored in the
    /// config, and similar.
    pub fn parse(src: &str) -> Result<Loaded, ConfigError> {
        let doc = parse_doc(src).map_err(|e| ConfigError {
            path: None,
            key: None,
            line: e.line,
            message: e.message,
        })?;
        let ix = Index::build(src, &doc, true);
        reject_secrets(src, &doc)?;
        check_types(src, &doc)?;
        let warnings = collect_warnings(src, &doc);
        let config: Config = toml::from_str(src).map_err(|e| {
            let key = e.span().and_then(|s| ix.path_at(s.start));
            let line = e.span().map(|s| line_of_offset(src, s.start));
            ConfigError { path: None, key, line, message: value_free(e.message()) }
        })?;
        config.validate(&ix)?;
        Ok(Loaded { config, warnings })
    }

    /// Read and parse a file. A missing file gives the defaults and no warnings.
    ///
    /// # Errors
    /// Returns a [`ConfigError`] (with `path` set) when the file cannot be
    /// read or fails [`Config::parse`].
    pub fn load(path: &Path) -> Result<Loaded, ConfigError> {
        let defaults = |warning: Option<String>| Loaded {
            config: Config::default(),
            warnings: warning
                .map(|message| ConfigWarning { key: "config".into(), line: None, message })
                .into_iter()
                .collect(),
        };
        let bytes = match read_regular(path) {
            Ok(b) => b,
            Err(Refusal::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(defaults(None));
            }
            Err(Refusal::Io(e)) => {
                return Err(ConfigError::io(path, format!("cannot read config file: {e}")));
            }
            // A FIFO, directory, device or oversized file: never opened blockingly, never read
            // whole. Treated as unreadable: the defaults, and a warning naming what it is.
            Err(refusal) => {
                return Ok(defaults(Some(format!(
                    "the config file {}; using the defaults",
                    refusal.phrase()
                ))));
            }
        };
        let src = String::from_utf8(bytes).map_err(|e| {
            ConfigError::io(path, format!("cannot read config file: {}", e.utf8_error()))
        })?;
        Self::parse(&src).map_err(|mut e| {
            e.path = Some(path.to_path_buf());
            e
        })
    }

    fn validate(&self, ix: &Index) -> Result<(), ConfigError> {
        let range =
            |section: &str, key: &str, v: f64, lo: f64, hi: f64| -> Result<(), ConfigError> {
                if v.is_nan() || v < lo || v > hi {
                    Err(ConfigError::at(
                        &format!("{section}.{key}"),
                        ix.line(&format!("{section}.{key}")),
                        format!("is out of range; expected {lo} to {hi}"),
                    ))
                } else {
                    Ok(())
                }
            };
        let (f, w, t) = (&self.font, &self.window, &self.terminal);
        if f.family.trim().is_empty() {
            return Err(ConfigError::at(
                "font.family",
                ix.line("font.family"),
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
                ix.line("terminal.shell.program"),
                "must not be empty",
            ));
        }
        self.validate_theme(ix)?;
        for (i, p) in self.ai.providers.iter().enumerate() {
            self.validate_provider(ix, i, p)?;
        }
        for (i, name) in self.agents.detect.iter().enumerate() {
            if name.trim().is_empty() {
                return Err(ConfigError::at(
                    &format!("agents.detect[{i}]"),
                    ix.line("agents.detect"),
                    "process names must not be empty",
                ));
            }
        }
        self.keymap_checked(Platform::Mac, Some(ix))?;
        self.keymap_checked(Platform::Linux, Some(ix))?;
        Ok(())
    }

    fn validate_theme(&self, ix: &Index) -> Result<(), ConfigError> {
        // `light` and `dark` default to the bundled pair, so a conflict can only
        // be seen from what the user actually wrote.
        let written = |k: &str| {
            let p = format!("theme.{k}");
            ix.has(&p).then(|| ix.line(&p)).flatten()
        };
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
        ix: &Index,
        i: usize,
        p: &ProviderConfig,
    ) -> Result<(), ConfigError> {
        let key = |k: &str| format!("ai.providers[{i}].{k}");
        let l = |k: &str| ix.line(&key(k));
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

    fn keymap_checked(
        &self,
        platform: Platform,
        ix: Option<&Index>,
    ) -> Result<Keymap, ConfigError> {
        let mut map = Keymap::defaults(platform);
        let mut seen: BTreeMap<KeyChord, &str> = BTreeMap::new();
        for (chord_s, action_s) in &self.keybindings {
            let key = format!("keybindings.{chord_s}");
            let line = ix.and_then(|ix| ix.line(&key));
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
                            "the value is not an action (it is not shown); use `none` or one of: {}",
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
        self.keymap_checked(platform, None)
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
