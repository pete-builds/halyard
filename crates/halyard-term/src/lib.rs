//! Terminal sessions for Halyard.
//!
//! This crate owns everything below the UI: the PTY, the VT engine, and the
//! [`frame::Frame`] snapshots the renderer draws. It has no UI dependencies.

pub mod frame;

pub use frame::{Cell, CellFlags, CellText, Cursor, CursorShape, Frame, Rgb, ScrollInfo};
