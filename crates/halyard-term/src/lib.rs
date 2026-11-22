//! Terminal sessions for Halyard.
//!
//! This crate owns everything below the UI: the PTY, the VT engine, and the
//! [`frame::Frame`] snapshots the renderer draws. It has no UI dependencies.
//!
//! - [`Session`] runs a program in a PTY on its own threads and publishes
//!   frames. This is what the UI uses.
//! - [`Engine`] is the headless VT engine (libghostty-vt) behind a session.
//!   It is the only code that touches libghostty-vt.
//! - [`flatpak`] decides how a session starts its program on the host when
//!   Halyard itself runs inside a Flatpak sandbox.

pub mod engine;
pub mod flatpak;
pub mod frame;
pub mod input;
pub mod palette;
pub mod session;

pub use engine::{Engine, EngineEffects, EngineError, paste_is_safe};
pub use frame::{Cell, CellFlags, CellText, Cursor, CursorShape, Frame, Rgb, ScrollInfo};
pub use input::{
    KeyAction, KeyCode, KeyInput, Modifiers, MouseAction, MouseButton, MouseInput, ScrollRequest,
};
pub use palette::Palette;
pub use session::{Session, SessionConfig, SessionError, SessionEvent, default_shell};
