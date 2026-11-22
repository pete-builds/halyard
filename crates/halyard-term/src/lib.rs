//! Terminal sessions for Halyard.
//!
//! This crate owns everything below the UI: the PTY, the VT engine, and the
//! [`frame::Frame`] snapshots the renderer draws. It has no UI dependencies.
//!
//! - [`Session`] runs a program in a PTY on its own threads and publishes
//!   frames. This is what the UI uses.
//! - [`Engine`] is the headless VT engine (libghostty-vt) behind a session.
//!   It is the only code that touches libghostty-vt.

pub mod engine;
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
pub use session::{
    OutputObserver, Session, SessionConfig, SessionError, SessionEvent, default_shell,
    new_session_id,
};
