//! Configuration and themes for Halyard.
//!
//! * [`config`]: `config.toml` parsing, validation, location and the commented default.
//! * [`keys`]: key chords, actions and the per-platform default keymap.
//! * [`theme`]: token-based themes, bundled palettes, contrast checking, import and export.
//! * [`watcher`]: live reload of the config file.
//!
//! This crate is a library only. It reads files and watches one path; it makes no
//! network calls and never stores secrets.

pub mod config;
mod fsread;
pub mod keys;
mod locate;
pub mod theme;
pub mod watcher;

pub use config::{
    AgentsConfig, AiConfig, Config, ConfigError, ConfigWarning, CursorStyle, Decorations,
    FontConfig, Loaded, ProviderConfig, ProviderKind, SessionApiConfig, ShellConfig,
    TerminalConfig, ThemeSelection, WindowConfig, WorkingDirectory, config_path, config_path_with,
    default_config_toml, themes_dir_for,
};
pub use keys::{Action, ChordError, Key, KeyChord, Keymap, Mods, NamedKey, Platform};
pub use theme::{
    Appearance, ContrastFailure, Entry, Origin, TerminalColors, TerminalPalette, Theme, ThemeError,
    ThemeRegistry, ThemeWarning, UiColors, check_contrast, contrast_ratio, derive_ui,
    relative_luminance,
};
pub use watcher::{ConfigEvent, ConfigWatcher};
