//! Mapping GPUI keystrokes to terminal input.

use gpui::Keystroke;
use halyard_term::{KeyAction, KeyCode, KeyInput, Modifiers};

/// Where a keystroke should go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyRoute {
    /// Encode and send this key to the terminal now.
    Terminal(KeyInput),
    /// Let the platform text input deliver the text (so IME composition
    /// and dead keys work). The terminal receives it as committed text.
    Text {
        /// The key, for the Kitty protocol's key code.
        key: KeyCode,
        /// Modifiers held (only Shift, or Option when it composes text).
        mods: Modifiers,
    },
    /// Not for the terminal: an application shortcut (Cmd on macOS, Super
    /// elsewhere) or a bare modifier press.
    Ignore,
}

fn named_key(key: &str) -> Option<KeyCode> {
    Some(match key {
        "enter" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "backspace" => KeyCode::Backspace,
        "escape" => KeyCode::Escape,
        "space" => KeyCode::Space,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        "insert" => KeyCode::Insert,
        "delete" => KeyCode::Delete,
        _ => {
            let n: u8 = key.strip_prefix('f')?.parse().ok()?;
            if !(1..=25).contains(&n) {
                return None;
            }
            KeyCode::F(n)
        }
    })
}

/// Whether a key the user pressed in a pane ends or clears the command line:
/// Enter, Ctrl-C or Ctrl-U (without Alt). After one of these the user has seen
/// the line, so the session API forgets what a paired tool typed there under
/// standing permission (`IpcServer::reset_typed_since_approval`).
///
/// Only real key events from the window may call this. Text a tool sends, pastes
/// and the terminal's own query replies go through other paths and never reset
/// it (`session_api` tests prove that for `session.send_text`).
#[must_use]
pub fn resets_typed_line(key: &KeyInput) -> bool {
    let m = key.mods;
    if m.alt || m.super_key {
        return false;
    }
    match key.key {
        KeyCode::Enter => !m.ctrl,
        KeyCode::Char('c' | 'u') => m.ctrl && !m.shift,
        _ => false,
    }
}

/// Decide how a key-down should reach the terminal.
///
/// `option_as_alt` only matters on macOS: when false, Option composes
/// characters (Option+s types `ß`) instead of acting as Alt.
#[must_use]
pub fn route_keystroke(ks: &Keystroke, option_as_alt: bool) -> KeyRoute {
    let m = ks.modifiers;
    if m.platform {
        return KeyRoute::Ignore;
    }
    let mods = Modifiers { shift: m.shift, ctrl: m.control, alt: m.alt, super_key: false };
    let alt_is_meta = m.alt && (option_as_alt || !cfg!(target_os = "macos"));

    if let Some(code) = named_key(&ks.key) {
        // Space is the only named key that types text.
        if code == KeyCode::Space && !m.control && !alt_is_meta {
            let mods = Modifiers { alt: false, ..mods };
            return KeyRoute::Terminal(KeyInput {
                key: code,
                text: Some(" ".to_owned()),
                mods,
                action: KeyAction::Press,
            });
        }
        return KeyRoute::Terminal(KeyInput {
            key: code,
            text: None,
            mods,
            action: KeyAction::Press,
        });
    }

    let mut chars = ks.key.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        // Multi-character names that are not keys we know (bare modifiers,
        // media keys) are not terminal input.
        return KeyRoute::Ignore;
    };
    let key = KeyCode::Char(c.to_ascii_lowercase());

    // AltGr on Linux arrives as Ctrl+Alt with the layout's character (for
    // example Ctrl+Alt+Q gives "@" on a German layout). That is typed text,
    // not a Ctrl or Alt combination.
    if m.control
        && m.alt
        && let Some(text) = ks.key_char.as_deref()
        && let Some(first) = text.chars().next()
        && !text.chars().any(char::is_control)
        && !text.eq_ignore_ascii_case(&ks.key)
    {
        let mods = Modifiers { shift: m.shift, ..Modifiers::NONE };
        return KeyRoute::Terminal(KeyInput {
            key: KeyCode::Char(first),
            text: Some(text.to_owned()),
            mods,
            action: KeyAction::Press,
        });
    }

    if m.control || alt_is_meta {
        // The encoder derives Ctrl and Alt sequences from the logical key;
        // for Alt it wants the unmodified character as text.
        let text =
            (!m.control).then(|| if m.shift { c.to_uppercase().collect() } else { c.to_string() });
        return KeyRoute::Terminal(KeyInput { key, text, mods, action: KeyAction::Press });
    }

    KeyRoute::Text { key, mods }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ks(key: &str, f: impl FnOnce(&mut gpui::Modifiers), key_char: Option<&str>) -> Keystroke {
        let mut modifiers = gpui::Modifiers::default();
        f(&mut modifiers);
        Keystroke { modifiers, key: key.to_owned(), key_char: key_char.map(str::to_owned) }
    }

    fn terminal(route: KeyRoute) -> KeyInput {
        match route {
            KeyRoute::Terminal(k) => k,
            other => panic!("expected a terminal key, got {other:?}"),
        }
    }

    #[test]
    fn named_keys_go_to_the_terminal() {
        let table = [
            ("enter", KeyCode::Enter),
            ("tab", KeyCode::Tab),
            ("backspace", KeyCode::Backspace),
            ("escape", KeyCode::Escape),
            ("up", KeyCode::Up),
            ("down", KeyCode::Down),
            ("left", KeyCode::Left),
            ("right", KeyCode::Right),
            ("home", KeyCode::Home),
            ("end", KeyCode::End),
            ("pageup", KeyCode::PageUp),
            ("pagedown", KeyCode::PageDown),
            ("insert", KeyCode::Insert),
            ("delete", KeyCode::Delete),
            ("f1", KeyCode::F(1)),
            ("f12", KeyCode::F(12)),
            ("f25", KeyCode::F(25)),
        ];
        for (name, code) in table {
            let k = terminal(route_keystroke(&ks(name, |_| {}, None), false));
            assert_eq!(k.key, code, "{name}");
            assert_eq!(k.text, None, "{name}");
        }
        assert_eq!(route_keystroke(&ks("f26", |_| {}, None), false), KeyRoute::Ignore);
    }

    #[test]
    fn space_types_a_space() {
        let k = terminal(route_keystroke(&ks("space", |_| {}, Some(" ")), false));
        assert_eq!((k.key, k.text.as_deref()), (KeyCode::Space, Some(" ")));
        let k = terminal(route_keystroke(&ks("space", |m| m.control = true, None), false));
        assert_eq!((k.key, k.text, k.mods.ctrl), (KeyCode::Space, None, true));
    }

    #[test]
    fn ctrl_combos_go_to_the_terminal_without_text() {
        let k = terminal(route_keystroke(&ks("c", |m| m.control = true, None), false));
        assert_eq!(k.key, KeyCode::Char('c'));
        assert_eq!(k.text, None);
        assert!(k.mods.ctrl);
    }

    #[test]
    fn modified_named_keys_keep_modifiers() {
        let k = terminal(route_keystroke(&ks("left", |m| m.alt = true, None), false));
        assert_eq!(k.key, KeyCode::Left);
        assert!(k.mods.alt);
        let k = terminal(route_keystroke(&ks("tab", |m| m.shift = true, None), false));
        assert!(k.mods.shift);
    }

    #[test]
    fn printable_keys_go_through_text_input() {
        assert_eq!(
            route_keystroke(&ks("a", |_| {}, Some("a")), false),
            KeyRoute::Text { key: KeyCode::Char('a'), mods: Modifiers::NONE }
        );
        assert_eq!(
            route_keystroke(&ks("a", |m| m.shift = true, Some("A")), false),
            KeyRoute::Text { key: KeyCode::Char('a'), mods: Modifiers::SHIFT }
        );
    }

    #[test]
    fn platform_shortcuts_are_reserved_for_the_app() {
        for key in ["c", "v", "q", "n", "k"] {
            assert_eq!(
                route_keystroke(&ks(key, |m| m.platform = true, None), false),
                KeyRoute::Ignore
            );
        }
        assert_eq!(
            route_keystroke(&ks("shift", |m| m.shift = true, None), false),
            KeyRoute::Ignore
        );
    }

    #[test]
    fn altgr_types_the_layout_character() {
        let ks = ks(
            "q",
            |m| {
                m.control = true;
                m.alt = true;
            },
            Some("@"),
        );
        let k = terminal(route_keystroke(&ks, false));
        assert_eq!(k.text.as_deref(), Some("@"));
        assert!(!k.mods.ctrl && !k.mods.alt, "{k:?}");
        // And the engine turns it into the character, not ESC NUL.
        let mut engine =
            halyard_term::Engine::new(10, 2, 0, &halyard_term::Palette::default()).unwrap();
        assert_eq!(engine.encode_key(&k).unwrap(), b"@");
    }

    #[test]
    fn ctrl_alt_without_layout_text_stays_a_combo() {
        let ks = ks(
            "q",
            |m| {
                m.control = true;
                m.alt = true;
            },
            None,
        );
        let k = terminal(route_keystroke(&ks, true));
        assert!(k.mods.ctrl && k.mods.alt);
        assert_eq!(k.text, None);
    }

    #[test]
    fn ctrl_alt_with_a_control_character_stays_a_combo() {
        // Some layouts report Ctrl+Alt+Q with the control character as its
        // text; that is not AltGr typing a character.
        let ks = ks(
            "q",
            |m| {
                m.control = true;
                m.alt = true;
            },
            Some("\u{11}"),
        );
        let k = terminal(route_keystroke(&ks, false));
        assert!(k.mods.ctrl && k.mods.alt, "{k:?}");
        assert_eq!(k.key, KeyCode::Char('q'));
        assert_eq!(k.text, None);
    }

    #[test]
    fn ctrl_alt_reporting_the_key_itself_stays_a_combo() {
        for text in ["q", "Q"] {
            let ks = ks(
                "q",
                |m| {
                    m.control = true;
                    m.alt = true;
                },
                Some(text),
            );
            let k = terminal(route_keystroke(&ks, false));
            assert!(k.mods.ctrl && k.mods.alt, "{text}: {k:?}");
            assert_eq!(k.text, None, "{text}");
        }
    }

    #[test]
    fn alt_is_meta_when_option_as_alt() {
        let k = terminal(route_keystroke(&ks("b", |m| m.alt = true, Some("∫")), true));
        assert_eq!(k.key, KeyCode::Char('b'));
        assert_eq!(k.text.as_deref(), Some("b"));
        assert!(k.mods.alt);
    }

    #[test]
    fn option_composes_on_macos_by_default() {
        let route = route_keystroke(&ks("s", |m| m.alt = true, Some("ß")), false);
        if cfg!(target_os = "macos") {
            assert!(matches!(route, KeyRoute::Text { .. }), "{route:?}");
        } else {
            assert!(matches!(route, KeyRoute::Terminal(_)), "{route:?}");
        }
    }

    #[test]
    fn only_enter_ctrl_c_and_ctrl_u_reset_the_typed_line() {
        let route =
            |key: &str, f: fn(&mut gpui::Modifiers), text: Option<&str>| match route_keystroke(
                &ks(key, f, text),
                false,
            ) {
                KeyRoute::Terminal(k) => Some(k),
                _ => None,
            };
        let enter = route("enter", |_| {}, None).unwrap();
        assert!(resets_typed_line(&enter));
        assert!(resets_typed_line(&route("c", |m| m.control = true, None).unwrap()));
        assert!(resets_typed_line(&route("u", |m| m.control = true, None).unwrap()));
        for (key, f) in [
            ("a", (|m: &mut gpui::Modifiers| m.control = true) as fn(&mut gpui::Modifiers)),
            ("d", |m| m.control = true),
            ("j", |m| m.control = true),
            ("c", |m| m.alt = true),
            ("u", |m| {
                m.control = true;
                m.alt = true;
            }),
            ("tab", |_| {}),
            ("backspace", |_| {}),
            ("enter", |m| m.alt = true),
        ] {
            if let Some(k) = route(key, f, None) {
                assert!(!resets_typed_line(&k), "{key} {:?}", k.mods);
            }
        }
        // Typed text (the Text route) never reaches this check at all.
        assert!(route("a", |_| {}, Some("a")).is_none());
    }
}
