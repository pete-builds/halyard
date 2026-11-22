//! Input types the UI sends to a session.
//!
//! These are Halyard's own types. The engine adapter translates them into
//! libghostty-vt key and mouse events, so the UI never depends on the engine.

/// Modifier keys held during an input event.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Modifiers {
    /// Shift.
    pub shift: bool,
    /// Control.
    pub ctrl: bool,
    /// Alt (Option on macOS).
    pub alt: bool,
    /// Super (Command on macOS, Windows key elsewhere).
    pub super_key: bool,
}

impl Modifiers {
    /// No modifiers.
    pub const NONE: Self = Self { shift: false, ctrl: false, alt: false, super_key: false };
    /// Control only.
    pub const CTRL: Self = Self { shift: false, ctrl: true, alt: false, super_key: false };
    /// Shift only.
    pub const SHIFT: Self = Self { shift: true, ctrl: false, alt: false, super_key: false };
    /// Alt only.
    pub const ALT: Self = Self { shift: false, ctrl: false, alt: true, super_key: false };

    /// True if no modifier is held.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        !(self.shift || self.ctrl || self.alt || self.super_key)
    }
}

/// A key on the keyboard, named by what it does rather than by scancode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KeyCode {
    /// A key that produces a character. Use the unshifted character
    /// (`'a'`, not `'A'`; `'1'`, not `'!'`).
    Char(char),
    /// Enter or Return.
    Enter,
    /// Tab.
    Tab,
    /// Backspace.
    Backspace,
    /// Escape.
    Escape,
    /// Space bar.
    Space,
    /// Up arrow.
    Up,
    /// Down arrow.
    Down,
    /// Left arrow.
    Left,
    /// Right arrow.
    Right,
    /// Home.
    Home,
    /// End.
    End,
    /// Page Up.
    PageUp,
    /// Page Down.
    PageDown,
    /// Insert.
    Insert,
    /// Forward delete.
    Delete,
    /// Function key F1 to F25.
    F(u8),
}

/// Whether a key went down, repeated, or came up.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum KeyAction {
    /// Key pressed.
    #[default]
    Press,
    /// Key held and auto-repeating.
    Repeat,
    /// Key released. Only reported to applications that asked for release
    /// events through the Kitty keyboard protocol.
    Release,
}

/// A keyboard event.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct KeyInput {
    /// The key.
    pub key: KeyCode,
    /// Text the keyboard layout produced for this key, before any Ctrl
    /// transformation (for Shift+a this is `"A"`). `None` for keys that
    /// produce no text, such as arrows or Ctrl combinations.
    pub text: Option<String>,
    /// Modifiers held.
    pub mods: Modifiers,
    /// Press, repeat or release.
    pub action: KeyAction,
}

impl KeyInput {
    /// A key press with no text.
    #[must_use]
    pub const fn new(key: KeyCode, mods: Modifiers) -> Self {
        Self { key, text: None, mods, action: KeyAction::Press }
    }

    /// A key press that produced `text`.
    #[must_use]
    pub fn with_text(key: KeyCode, text: impl Into<String>, mods: Modifiers) -> Self {
        Self { key, text: Some(text.into()), mods, action: KeyAction::Press }
    }
}

/// A mouse button, including wheel directions (which terminals report as
/// buttons 4 to 7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MouseButton {
    /// Primary button.
    Left,
    /// Middle button.
    Middle,
    /// Secondary button.
    Right,
    /// Wheel scrolled up.
    WheelUp,
    /// Wheel scrolled down.
    WheelDown,
    /// Wheel scrolled left.
    WheelLeft,
    /// Wheel scrolled right.
    WheelRight,
}

/// What the mouse did.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MouseAction {
    /// Button pressed.
    Press,
    /// Button released.
    Release,
    /// Pointer moved.
    Motion,
}

/// A mouse event in grid pixel coordinates: `(0, 0)` is the top-left corner
/// of the first cell, and positions use the same pixel unit as the cell size
/// passed to [`crate::Session::resize`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MouseInput {
    /// What happened.
    pub action: MouseAction,
    /// The button involved, if any (`None` for plain motion).
    pub button: Option<MouseButton>,
    /// Horizontal position in pixels.
    pub x: f32,
    /// Vertical position in pixels.
    pub y: f32,
    /// Modifiers held.
    pub mods: Modifiers,
    /// True while any button is held (distinguishes drags from motion).
    pub any_button_pressed: bool,
}

/// A request to move the viewport through scrollback.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScrollRequest {
    /// Scroll by rows; negative moves up into history.
    Lines(isize),
    /// Scroll by viewport heights; negative moves up into history.
    Pages(isize),
    /// Jump to the oldest line of scrollback.
    Top,
    /// Jump back to the live screen.
    Bottom,
    /// Put this row (counted from the top of scrollback, as
    /// [`crate::Engine::cursor_screen_row`] counts) at the top of the viewport,
    /// as far as the scrollback allows.
    Row(u64),
}
