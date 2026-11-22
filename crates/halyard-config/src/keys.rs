//! Key chords, actions and the default keymap.

use std::collections::BTreeMap;
use std::fmt;

/// The platform whose conventions apply to keybindings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    /// macOS: `primary` means Cmd.
    Mac,
    /// Linux and other Unix: `primary` means Ctrl+Shift, so plain Ctrl chords stay with the shell.
    Linux,
}

impl Platform {
    /// The platform this build runs on.
    #[must_use]
    pub fn current() -> Self {
        if cfg!(target_os = "macos") { Platform::Mac } else { Platform::Linux }
    }
}

/// Modifier keys held in a chord.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Mods {
    /// Control.
    pub ctrl: bool,
    /// Shift.
    pub shift: bool,
    /// Alt (Option on macOS).
    pub alt: bool,
    /// Cmd on macOS, Super elsewhere.
    pub cmd: bool,
}

/// A non-character key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NamedKey {
    /// Enter or Return.
    Enter,
    /// Tab.
    Tab,
    /// Escape.
    Escape,
    /// Space bar.
    Space,
    /// Backspace.
    Backspace,
    /// Forward delete.
    Delete,
    /// Insert.
    Insert,
    /// Arrow up.
    Up,
    /// Arrow down.
    Down,
    /// Arrow left.
    Left,
    /// Arrow right.
    Right,
    /// Home.
    Home,
    /// End.
    End,
    /// Page up.
    PageUp,
    /// Page down.
    PageDown,
    /// Function key `F1` to `F24`.
    F(u8),
}

/// The key part of a chord.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Key {
    /// A printable character, lowercased.
    Char(char),
    /// A named key.
    Named(NamedKey),
}

/// A modifier set plus one key, with `primary` already resolved for a platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeyChord {
    /// Held modifiers.
    pub mods: Mods,
    /// The key.
    pub key: Key,
}

/// Why a chord string was rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChordError(pub String);

impl fmt::Display for ChordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ChordError {}

fn named_key(token: &str) -> Option<NamedKey> {
    Some(match token {
        "enter" | "return" => NamedKey::Enter,
        "tab" => NamedKey::Tab,
        "escape" | "esc" => NamedKey::Escape,
        "space" => NamedKey::Space,
        "backspace" => NamedKey::Backspace,
        "delete" | "del" => NamedKey::Delete,
        "insert" => NamedKey::Insert,
        "up" => NamedKey::Up,
        "down" => NamedKey::Down,
        "left" => NamedKey::Left,
        "right" => NamedKey::Right,
        "home" => NamedKey::Home,
        "end" => NamedKey::End,
        "pageup" => NamedKey::PageUp,
        "pagedown" => NamedKey::PageDown,
        t => {
            let n: u8 = t.strip_prefix('f')?.parse().ok()?;
            if !(1..=24).contains(&n) || t.len() > 3 {
                return None;
            }
            NamedKey::F(n)
        }
    })
}

impl KeyChord {
    /// Parse a chord such as `cmd+shift+t`, `primary+c`, `ctrl+pagedown` or `f11`.
    ///
    /// Tokens are joined by `+`, are case-insensitive, and the last one is the
    /// key. Modifiers: `ctrl`, `shift`, `alt` (`opt`, `option`), `cmd`
    /// (`command`, `super`) and `primary`, which is Cmd on macOS and
    /// Ctrl+Shift elsewhere. Use `plus` for the `+` key. A character key needs
    /// a modifier other than Shift alone, so typing stays typing.
    ///
    /// # Errors
    /// Returns a [`ChordError`] saying what is wrong with the string.
    pub fn parse(s: &str, platform: Platform) -> Result<KeyChord, ChordError> {
        let err = |m: String| Err(ChordError(m));
        if s.trim().is_empty() {
            return err("chord is empty".into());
        }
        let tokens: Vec<String> = s.split('+').map(|t| t.trim().to_ascii_lowercase()).collect();
        if tokens.iter().any(String::is_empty) {
            return err(format!("`{s}` has an empty part; write the plus key as `plus`"));
        }
        let (key_tok, mod_toks) =
            tokens.split_last().expect("split of a non-empty string has a token");
        let mut mods = Mods::default();
        let mut seen: Vec<&str> = Vec::new();
        let apply = |tok: &str, mods: &mut Mods| -> bool {
            match tok {
                "ctrl" | "control" => mods.ctrl = true,
                "shift" => mods.shift = true,
                "alt" | "opt" | "option" => mods.alt = true,
                "cmd" | "command" | "super" => mods.cmd = true,
                "primary" => match platform {
                    Platform::Mac => mods.cmd = true,
                    Platform::Linux => {
                        mods.ctrl = true;
                        mods.shift = true;
                    }
                },
                _ => return false,
            }
            true
        };
        for tok in mod_toks {
            let canonical = match tok.as_str() {
                "control" => "ctrl",
                "opt" | "option" => "alt",
                "command" | "super" => "cmd",
                t => t,
            };
            if seen.contains(&canonical) {
                return err(format!("modifier `{tok}` appears twice in `{s}`"));
            }
            seen.push(canonical);
            if !apply(tok, &mut mods) {
                return err(format!(
                    "`{tok}` in `{s}` is not a modifier; use ctrl, shift, alt, cmd or primary"
                ));
            }
        }
        let mut probe = Mods::default();
        if apply(key_tok, &mut probe) {
            return err(format!("`{s}` has modifiers but no key"));
        }
        let key = if key_tok == "plus" {
            Key::Char('+')
        } else if let Some(n) = named_key(key_tok) {
            Key::Named(n)
        } else {
            let mut chars = key_tok.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if !c.is_whitespace() && !c.is_control() => Key::Char(c),
                _ => return err(format!("`{key_tok}` in `{s}` is not a known key")),
            }
        };
        let types_text = matches!(key, Key::Char(_) | Key::Named(NamedKey::Space));
        if types_text && !(mods.ctrl || mods.alt || mods.cmd) {
            return err(format!(
                "`{s}` would capture ordinary typing; add ctrl, alt, cmd or primary"
            ));
        }
        Ok(KeyChord { mods, key })
    }
}

impl fmt::Display for KeyChord {
    /// Canonical form: `ctrl+alt+shift+cmd+key`, lowercase.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let m = &self.mods;
        for (on, name) in [(m.ctrl, "ctrl"), (m.alt, "alt"), (m.shift, "shift"), (m.cmd, "cmd")] {
            if on {
                write!(f, "{name}+")?;
            }
        }
        match self.key {
            Key::Char('+') => f.write_str("plus"),
            Key::Char(c) => write!(f, "{c}"),
            Key::Named(n) => match n {
                NamedKey::Enter => f.write_str("enter"),
                NamedKey::Tab => f.write_str("tab"),
                NamedKey::Escape => f.write_str("escape"),
                NamedKey::Space => f.write_str("space"),
                NamedKey::Backspace => f.write_str("backspace"),
                NamedKey::Delete => f.write_str("delete"),
                NamedKey::Insert => f.write_str("insert"),
                NamedKey::Up => f.write_str("up"),
                NamedKey::Down => f.write_str("down"),
                NamedKey::Left => f.write_str("left"),
                NamedKey::Right => f.write_str("right"),
                NamedKey::Home => f.write_str("home"),
                NamedKey::End => f.write_str("end"),
                NamedKey::PageUp => f.write_str("pageup"),
                NamedKey::PageDown => f.write_str("pagedown"),
                NamedKey::F(n) => write!(f, "f{n}"),
            },
        }
    }
}

macro_rules! actions {
    ($($variant:ident => $name:literal),+ $(,)?) => {
        /// Something a key chord can trigger.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum Action { $(
            #[doc = concat!("The `", $name, "` action.")]
            $variant,
        )+ }

        impl Action {
            /// Every action, in documentation order.
            pub const ALL: &'static [Action] = &[$(Action::$variant),+];

            /// The snake_case name used in `config.toml`.
            #[must_use]
            pub fn name(self) -> &'static str {
                match self { $(Action::$variant => $name),+ }
            }

            /// Look an action up by its config name.
            #[must_use]
            pub fn from_name(s: &str) -> Option<Action> {
                match s { $($name => Some(Action::$variant),)+ _ => None }
            }
        }
    };
}

actions! {
    NewTab => "new_tab",
    ClosePane => "close_pane",
    SplitRight => "split_right",
    SplitDown => "split_down",
    FocusLeft => "focus_left",
    FocusRight => "focus_right",
    FocusUp => "focus_up",
    FocusDown => "focus_down",
    NextTab => "next_tab",
    PrevTab => "prev_tab",
    CommandPalette => "command_palette",
    Copy => "copy",
    Paste => "paste",
    Find => "find",
    ZoomPane => "zoom_pane",
    IncreaseFontSize => "increase_font_size",
    DecreaseFontSize => "decrease_font_size",
    ResetFontSize => "reset_font_size",
    AgentDashboard => "agent_dashboard",
    NewWindow => "new_window",
    Quit => "quit",
    ResizePaneLeft => "resize_pane_left",
    ResizePaneRight => "resize_pane_right",
    ResizePaneUp => "resize_pane_up",
    ResizePaneDown => "resize_pane_down",
    MoveTabLeft => "move_tab_left",
    MoveTabRight => "move_tab_right",
    RenameTab => "rename_tab",
    ReloadConfig => "reload_config",
    OpenConfig => "open_config",
    PrevPrompt => "prev_prompt",
    NextPrompt => "next_prompt",
    CopyLastOutput => "copy_last_output",
}

/// The value that removes a default binding when used as an action name.
pub const UNBIND: &str = "none";

/// Default chords for a platform, as `(chord string, action)`.
#[must_use]
pub fn default_bindings(platform: Platform) -> &'static [(&'static str, Action)] {
    use Action::*;
    match platform {
        Platform::Mac => &[
            ("cmd+t", NewTab),
            ("cmd+w", ClosePane),
            ("cmd+d", SplitRight),
            ("cmd+shift+d", SplitDown),
            ("cmd+alt+left", FocusLeft),
            ("cmd+alt+right", FocusRight),
            ("cmd+alt+up", FocusUp),
            ("cmd+alt+down", FocusDown),
            ("cmd+shift+]", NextTab),
            ("cmd+shift+[", PrevTab),
            ("cmd+shift+p", CommandPalette),
            ("cmd+c", Copy),
            ("cmd+v", Paste),
            ("cmd+f", Find),
            ("cmd+shift+enter", ZoomPane),
            ("cmd+=", IncreaseFontSize),
            ("cmd+-", DecreaseFontSize),
            ("cmd+0", ResetFontSize),
            ("cmd+shift+a", AgentDashboard),
            ("cmd+n", NewWindow),
            ("cmd+q", Quit),
            ("cmd+ctrl+left", ResizePaneLeft),
            ("cmd+ctrl+right", ResizePaneRight),
            ("cmd+ctrl+up", ResizePaneUp),
            ("cmd+ctrl+down", ResizePaneDown),
            ("cmd+shift+alt+left", MoveTabLeft),
            ("cmd+shift+alt+right", MoveTabRight),
            ("cmd+shift+i", RenameTab),
            ("cmd+shift+,", ReloadConfig),
            ("cmd+,", OpenConfig),
            ("cmd+up", PrevPrompt),
            ("cmd+down", NextPrompt),
            ("cmd+alt+c", CopyLastOutput),
        ],
        Platform::Linux => &[
            ("primary+t", NewTab),
            ("primary+w", ClosePane),
            ("primary+d", SplitRight),
            ("primary+e", SplitDown),
            ("alt+left", FocusLeft),
            ("alt+right", FocusRight),
            ("alt+up", FocusUp),
            ("alt+down", FocusDown),
            ("ctrl+pagedown", NextTab),
            ("ctrl+pageup", PrevTab),
            ("primary+p", CommandPalette),
            ("primary+c", Copy),
            ("primary+v", Paste),
            ("primary+f", Find),
            ("primary+enter", ZoomPane),
            ("primary+=", IncreaseFontSize),
            ("primary+-", DecreaseFontSize),
            ("primary+0", ResetFontSize),
            ("primary+a", AgentDashboard),
            ("primary+n", NewWindow),
            ("primary+q", Quit),
            ("primary+alt+left", ResizePaneLeft),
            ("primary+alt+right", ResizePaneRight),
            ("primary+alt+up", ResizePaneUp),
            ("primary+alt+down", ResizePaneDown),
            ("ctrl+shift+pageup", MoveTabLeft),
            ("ctrl+shift+pagedown", MoveTabRight),
            ("primary+i", RenameTab),
            ("primary+,", ReloadConfig),
            ("ctrl+,", OpenConfig),
            ("primary+up", PrevPrompt),
            ("primary+down", NextPrompt),
            ("primary+alt+c", CopyLastOutput),
        ],
    }
}

/// The resolved chord-to-action table: defaults plus the user's overrides.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keymap {
    bindings: BTreeMap<KeyChord, Action>,
}

impl Keymap {
    /// The default keymap for a platform.
    ///
    /// # Panics
    /// Never: a unit test parses every built-in default on both platforms.
    #[must_use]
    pub fn defaults(platform: Platform) -> Self {
        let bindings = default_bindings(platform)
            .iter()
            .map(|(c, a)| {
                (KeyChord::parse(c, platform).expect("built-in default chord parses"), *a)
            })
            .collect();
        Self { bindings }
    }

    /// Apply a user override: bind `chord` to `action`, or remove it with `None`.
    pub fn set(&mut self, chord: KeyChord, action: Option<Action>) {
        match action {
            Some(a) => {
                self.bindings.insert(chord, a);
            }
            None => {
                self.bindings.remove(&chord);
            }
        }
    }

    /// The action bound to a chord.
    #[must_use]
    pub fn action_for(&self, chord: &KeyChord) -> Option<Action> {
        self.bindings.get(chord).copied()
    }

    /// Every chord bound to an action.
    #[must_use]
    pub fn chords_for(&self, action: Action) -> Vec<KeyChord> {
        self.bindings.iter().filter(|(_, a)| **a == action).map(|(c, _)| *c).collect()
    }

    /// All bindings in chord order.
    pub fn iter(&self) -> impl Iterator<Item = (&KeyChord, &Action)> {
        self.bindings.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Result<KeyChord, ChordError> {
        KeyChord::parse(s, Platform::Mac)
    }

    #[test]
    fn parses_good_chords() {
        let c = p("Cmd+Shift+T").unwrap();
        assert_eq!(c.mods, Mods { ctrl: false, shift: true, alt: false, cmd: true });
        assert_eq!(c.key, Key::Char('t'));
        assert_eq!(p("ctrl+pagedown").unwrap().key, Key::Named(NamedKey::PageDown));
        assert_eq!(p("f11").unwrap().key, Key::Named(NamedKey::F(11)));
        assert_eq!(p("cmd+plus").unwrap().key, Key::Char('+'));
        assert_eq!(p("option+left").unwrap(), p("alt+left").unwrap());
        assert_eq!(p("cmd+shift+]").unwrap().key, Key::Char(']'));
        assert_eq!(p("ctrl+alt+shift+cmd+x").unwrap().to_string(), "ctrl+alt+shift+cmd+x");
    }

    #[test]
    fn primary_resolves_per_platform() {
        let mac = KeyChord::parse("primary+c", Platform::Mac).unwrap();
        assert_eq!(mac, KeyChord::parse("cmd+c", Platform::Mac).unwrap());
        let lin = KeyChord::parse("primary+c", Platform::Linux).unwrap();
        assert_eq!(lin, KeyChord::parse("ctrl+shift+c", Platform::Linux).unwrap());
    }

    #[test]
    fn rejects_bad_chords_with_reasons() {
        let cases = [
            ("", "empty"),
            ("cmd+", "empty part"),
            ("cmd++", "empty part"),
            ("cmd+shift", "no key"),
            ("cmd+cmd+t", "twice"),
            ("command+cmd+t", "twice"),
            ("hyper+t", "not a modifier"),
            ("cmd+nope", "not a known key"),
            ("cmd+f25", "not a known key"),
            ("cmd+f0", "not a known key"),
            ("t", "ordinary typing"),
            ("shift+t", "ordinary typing"),
            ("space", "ordinary typing"),
        ];
        for (input, needle) in cases {
            let e = p(input).expect_err(input);
            assert!(e.0.contains(needle), "{input:?}: {e}");
        }
    }

    #[test]
    fn named_and_function_keys_may_be_bare() {
        assert!(p("f5").is_ok());
        assert!(p("pagedown").is_ok());
        assert!(p("escape").is_ok());
    }

    #[test]
    fn action_names_round_trip() {
        for a in Action::ALL {
            assert_eq!(Action::from_name(a.name()), Some(*a));
        }
        assert_eq!(Action::from_name("nope"), None);
        assert_eq!(Action::ALL.len(), 33);
    }

    #[test]
    fn every_action_has_a_default_on_every_platform() {
        for platform in [Platform::Mac, Platform::Linux] {
            let map = Keymap::defaults(platform);
            for a in Action::ALL {
                assert!(
                    !map.chords_for(*a).is_empty(),
                    "{platform:?} lacks a default for {}",
                    a.name()
                );
            }
            assert_eq!(
                map.iter().count(),
                default_bindings(platform).len(),
                "{platform:?} has a duplicate default"
            );
        }
    }

    #[test]
    fn defaults_differ_between_platforms() {
        let mac = Keymap::defaults(Platform::Mac);
        let lin = Keymap::defaults(Platform::Linux);
        let copy_mac = mac.chords_for(Action::Copy)[0];
        let copy_lin = lin.chords_for(Action::Copy)[0];
        assert!(copy_mac.mods.cmd && !copy_mac.mods.ctrl);
        assert!(copy_lin.mods.ctrl && copy_lin.mods.shift && !copy_lin.mods.cmd);
    }

    #[test]
    fn set_overrides_and_unbinds() {
        let mut m = Keymap::defaults(Platform::Mac);
        let t = p("cmd+t").unwrap();
        m.set(t, Some(Action::Quit));
        assert_eq!(m.action_for(&t), Some(Action::Quit));
        m.set(t, None);
        assert_eq!(m.action_for(&t), None);
    }
}
