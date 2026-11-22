//! Config keymap to GPUI key bindings.
//!
//! Every `[keybindings]` action has a GPUI action of the same meaning. GPUI
//! matches bindings before any key listener runs, so a bound chord never
//! reaches the terminal, and menus show the chords next to their items.

use gpui::{DummyKeyboardMapper, KeyBinding, Keystroke, actions};
use halyard_config::{Action, Key, KeyChord, Keymap, NamedKey};

actions!(
    halyard,
    [
        NewTab,
        ClosePane,
        SplitRight,
        SplitDown,
        FocusLeft,
        FocusRight,
        FocusUp,
        FocusDown,
        NextTab,
        PrevTab,
        CommandPalette,
        Copy,
        Paste,
        ZoomPane,
        IncreaseFontSize,
        DecreaseFontSize,
        ResetFontSize,
        NewWindow,
        Quit,
        ResizePaneLeft,
        ResizePaneRight,
        ResizePaneUp,
        ResizePaneDown,
        MoveTabLeft,
        MoveTabRight,
        RenameTab,
        ReloadConfig,
        OpenConfig,
        SwitchTheme,
    ]
);

/// The GPUI action for a config action, or `None` for actions that are not
/// built yet (`find`, `agent_dashboard`), whose chords then stay with the
/// terminal.
#[must_use]
pub fn gpui_action(a: Action) -> Option<Box<dyn gpui::Action>> {
    Some(match a {
        Action::NewTab => Box::new(NewTab),
        Action::ClosePane => Box::new(ClosePane),
        Action::SplitRight => Box::new(SplitRight),
        Action::SplitDown => Box::new(SplitDown),
        Action::FocusLeft => Box::new(FocusLeft),
        Action::FocusRight => Box::new(FocusRight),
        Action::FocusUp => Box::new(FocusUp),
        Action::FocusDown => Box::new(FocusDown),
        Action::NextTab => Box::new(NextTab),
        Action::PrevTab => Box::new(PrevTab),
        Action::CommandPalette => Box::new(CommandPalette),
        Action::Copy => Box::new(Copy),
        Action::Paste => Box::new(Paste),
        Action::ZoomPane => Box::new(ZoomPane),
        Action::IncreaseFontSize => Box::new(IncreaseFontSize),
        Action::DecreaseFontSize => Box::new(DecreaseFontSize),
        Action::ResetFontSize => Box::new(ResetFontSize),
        Action::NewWindow => Box::new(NewWindow),
        Action::Quit => Box::new(Quit),
        Action::ResizePaneLeft => Box::new(ResizePaneLeft),
        Action::ResizePaneRight => Box::new(ResizePaneRight),
        Action::ResizePaneUp => Box::new(ResizePaneUp),
        Action::ResizePaneDown => Box::new(ResizePaneDown),
        Action::MoveTabLeft => Box::new(MoveTabLeft),
        Action::MoveTabRight => Box::new(MoveTabRight),
        Action::RenameTab => Box::new(RenameTab),
        Action::ReloadConfig => Box::new(ReloadConfig),
        Action::OpenConfig => Box::new(OpenConfig),
        Action::Find | Action::AgentDashboard => return None,
    })
}

/// The character a US layout types for Shift plus `c`, for the punctuation
/// GPUI may report as the shifted character rather than as Shift plus the
/// base key.
fn shifted(c: char) -> Option<char> {
    let pairs = [
        ('[', '{'),
        (']', '}'),
        ('=', '+'),
        ('-', '_'),
        (',', '<'),
        ('.', '>'),
        ('/', '?'),
        (';', ':'),
        ('\'', '"'),
        ('`', '~'),
        ('\\', '|'),
        ('1', '!'),
        ('2', '@'),
        ('3', '#'),
        ('4', '$'),
        ('5', '%'),
        ('6', '^'),
        ('7', '&'),
        ('8', '*'),
        ('9', '('),
        ('0', ')'),
    ];
    pairs.iter().find(|(b, _)| *b == c).map(|(_, s)| *s)
}

fn key_name(key: Key) -> String {
    match key {
        Key::Char(c) => c.to_string(),
        Key::Named(n) => match n {
            NamedKey::Enter => "enter".into(),
            NamedKey::Tab => "tab".into(),
            NamedKey::Escape => "escape".into(),
            NamedKey::Space => "space".into(),
            NamedKey::Backspace => "backspace".into(),
            NamedKey::Delete => "delete".into(),
            NamedKey::Insert => "insert".into(),
            NamedKey::Up => "up".into(),
            NamedKey::Down => "down".into(),
            NamedKey::Left => "left".into(),
            NamedKey::Right => "right".into(),
            NamedKey::Home => "home".into(),
            NamedKey::End => "end".into(),
            NamedKey::PageUp => "pageup".into(),
            NamedKey::PageDown => "pagedown".into(),
            NamedKey::F(n) => format!("f{n}"),
        },
    }
}

fn keystroke_text(ctrl: bool, alt: bool, shift: bool, cmd: bool, key: &str) -> String {
    let mut s = String::new();
    for (on, name) in [(ctrl, "ctrl-"), (alt, "alt-"), (shift, "shift-"), (cmd, "cmd-")] {
        if on {
            s.push_str(name);
        }
    }
    s.push_str(key);
    s
}

/// GPUI keystroke strings for a chord. Shift plus punctuation also gets the
/// shifted-character form (`cmd+shift+]` is also `cmd-}`), because platforms
/// differ in which one they report.
#[must_use]
pub fn keystrokes(chord: &KeyChord) -> Vec<String> {
    let m = chord.mods;
    let mut out = vec![keystroke_text(m.ctrl, m.alt, m.shift, m.cmd, &key_name(chord.key))];
    if m.shift
        && let Key::Char(c) = chord.key
        && let Some(s) = shifted(c)
    {
        out.push(keystroke_text(m.ctrl, m.alt, false, m.cmd, &s.to_string()));
    }
    out
}

/// Short label for a chord, for the palette and banners (`⌘⇧P`, `Ctrl+Shift+P`).
#[must_use]
pub fn label(chord: &KeyChord) -> String {
    let m = chord.mods;
    let key: String = match chord.key {
        Key::Char(c) => c.to_uppercase().collect(),
        Key::Named(n) => {
            let name = key_name(Key::Named(n));
            let mut c = name.chars();
            c.next().map(|f| f.to_uppercase().chain(c).collect::<String>()).unwrap_or_default()
        }
    };
    if cfg!(target_os = "macos") {
        let mut s = String::new();
        for (on, sym) in [(m.ctrl, "⌃"), (m.alt, "⌥"), (m.shift, "⇧"), (m.cmd, "⌘")] {
            if on {
                s.push_str(sym);
            }
        }
        s + &key
    } else {
        let mut parts: Vec<&str> = Vec::new();
        for (on, name) in [(m.ctrl, "Ctrl"), (m.alt, "Alt"), (m.shift, "Shift"), (m.cmd, "Super")] {
            if on {
                parts.push(name);
            }
        }
        let mut s = parts.join("+");
        if !s.is_empty() {
            s.push('+');
        }
        s + &key
    }
}

/// GPUI bindings for a keymap. Chords GPUI cannot parse are skipped and
/// returned as text, so the caller can report them.
#[must_use]
pub fn bindings(keymap: &Keymap) -> (Vec<KeyBinding>, Vec<String>) {
    let mut out = Vec::new();
    let mut skipped = Vec::new();
    for (chord, action) in keymap.iter() {
        for ks in keystrokes(chord) {
            let Some(a) = gpui_action(*action) else { continue };
            if Keystroke::parse(&ks).is_err() {
                skipped.push(chord.to_string());
                continue;
            }
            match KeyBinding::load(&ks, a, None, false, None, &DummyKeyboardMapper) {
                Ok(b) => out.push(b),
                Err(_) => skipped.push(chord.to_string()),
            }
        }
    }
    (out, skipped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use halyard_config::Platform;

    fn chord(s: &str, p: Platform) -> KeyChord {
        KeyChord::parse(s, p).unwrap()
    }

    #[test]
    fn chords_become_gpui_keystrokes() {
        assert_eq!(keystrokes(&chord("cmd+t", Platform::Mac)), vec!["cmd-t"]);
        assert_eq!(keystrokes(&chord("primary+t", Platform::Linux)), vec!["ctrl-shift-t"]);
        assert_eq!(keystrokes(&chord("cmd+shift+]", Platform::Mac)), vec!["shift-cmd-]", "cmd-}"]);
        assert_eq!(keystrokes(&chord("cmd+-", Platform::Mac)), vec!["cmd--"]);
        assert_eq!(keystrokes(&chord("alt+left", Platform::Linux)), vec!["alt-left"]);
        assert_eq!(keystrokes(&chord("ctrl+pagedown", Platform::Linux)), vec!["ctrl-pagedown"]);
        assert_eq!(keystrokes(&chord("cmd+shift+enter", Platform::Mac)), vec!["shift-cmd-enter"]);
    }

    #[test]
    fn every_default_chord_parses_as_a_gpui_keystroke() {
        for p in [Platform::Mac, Platform::Linux] {
            for (c, _) in Keymap::defaults(p).iter() {
                for ks in keystrokes(c) {
                    let parsed = Keystroke::parse(&ks).unwrap_or_else(|_| panic!("{ks}"));
                    assert_eq!(parsed.modifiers.platform, c.mods.cmd, "{ks}");
                    assert_eq!(parsed.modifiers.control, c.mods.ctrl, "{ks}");
                    assert_eq!(parsed.modifiers.alt, c.mods.alt, "{ks}");
                }
            }
        }
    }

    #[test]
    fn every_default_binding_is_loaded_except_unbuilt_actions() {
        for p in [Platform::Mac, Platform::Linux] {
            let map = Keymap::defaults(p);
            let (b, skipped) = bindings(&map);
            assert!(skipped.is_empty(), "{skipped:?}");
            let built = map.iter().filter(|(_, a)| gpui_action(**a).is_some()).count();
            assert!(b.len() >= built, "{} bindings for {built} chords", b.len());
        }
        assert!(gpui_action(Action::Find).is_none());
        assert!(gpui_action(Action::AgentDashboard).is_none());
        assert!(gpui_action(Action::SplitRight).is_some());
    }

    #[test]
    fn labels_are_readable() {
        let c = chord("cmd+shift+p", Platform::Mac);
        let l = label(&c);
        if cfg!(target_os = "macos") {
            assert_eq!(l, "⇧⌘P");
        } else {
            assert_eq!(l, "Shift+Super+P");
        }
        let c = chord("ctrl+pagedown", Platform::Linux);
        assert!(label(&c).ends_with("Pagedown"));
    }
}
