//! The app's view of `config.toml` and the theme registry.
//!
//! Plain data with no GPUI types, so it can be unit tested without a window.
//! The GPUI side keeps one [`Settings`] in a global and re-reads it whenever
//! the config watcher reports a change.

use std::path::{Path, PathBuf};

use halyard_config::{
    Appearance, Config, ConfigError, ConfigEvent, CursorStyle, Keymap, Loaded, Platform,
    TerminalPalette, Theme, ThemeRegistry, WorkingDirectory, themes_dir_for,
};
use halyard_term::{CursorShape, Palette};

/// Everything the windows need from the configuration, plus the problems
/// worth telling the user about.
#[derive(Clone, Debug)]
pub struct Settings {
    /// The configuration in effect. After a broken edit this is still the
    /// last good one.
    pub config: Config,
    /// Key chords for this platform.
    pub keymap: Keymap,
    /// Bundled plus user themes.
    pub registry: ThemeRegistry,
    /// Where `config.toml` lives, if a home directory is known.
    pub path: Option<PathBuf>,
    /// Warnings and errors from the last load, already phrased for people.
    /// Never holds a value from the file (see `docs/configuration.md`).
    pub notices: Vec<String>,
    /// A theme picked from the command palette for this run. It wins over
    /// `[theme]` until the `[theme]` section itself changes.
    pub theme_override: Option<String>,
    /// Bumped on every change, so views can tell a new state from an old one.
    pub generation: u64,
}

impl Settings {
    /// Built-in defaults, with no file.
    #[must_use]
    pub fn defaults() -> Self {
        let config = Config::default();
        Self {
            keymap: Keymap::defaults(Platform::current()),
            config,
            registry: ThemeRegistry::bundled(),
            path: None,
            notices: Vec::new(),
            theme_override: None,
            generation: 0,
        }
    }

    /// Load `path` (a missing file means defaults). Never fails: a broken
    /// file gives the defaults plus a notice that says why.
    #[must_use]
    pub fn load(path: Option<&Path>) -> Self {
        let mut s = Self::defaults();
        s.path = path.map(Path::to_path_buf);
        if let Some(p) = path {
            s.apply(Config::load(p));
        }
        s
    }

    /// Apply a reload result. `Ok` replaces the configuration and reloads the
    /// user themes. `Err` keeps the previous configuration and shows the
    /// error, so a half-saved edit never takes the terminal down.
    pub fn apply(&mut self, event: ConfigEvent) {
        self.generation += 1;
        match event {
            Ok(Loaded { config, warnings }) => {
                let mut notices: Vec<String> =
                    warnings.iter().map(|w| format!("config: {w}")).collect();
                let keymap = match config.keymap(Platform::current()) {
                    Ok(k) => k,
                    Err(e) => {
                        notices.push(describe_keymap_error(&e));
                        Keymap::defaults(Platform::current())
                    }
                };
                let mut registry = ThemeRegistry::bundled();
                if let Some(p) = &self.path {
                    for e in registry.load_user_dir(&themes_dir_for(p)) {
                        notices.push(format!("theme: {e}"));
                    }
                }
                if config.theme != self.config.theme {
                    self.theme_override = None;
                }
                self.config = config;
                self.keymap = keymap;
                self.registry = registry;
                self.notices = notices;
            }
            Err(e) => {
                self.notices = vec![describe_error(&e)];
            }
        }
        for appearance in [Appearance::Dark, Appearance::Light] {
            if let (_, Some(n)) = self.theme(appearance)
                && !self.notices.contains(&n)
            {
                self.notices.push(n);
            }
        }
    }

    /// The theme for a system appearance, and a notice when the configured
    /// name is unknown and a default was used instead.
    #[must_use]
    pub fn theme(&self, appearance: Appearance) -> (&Theme, Option<String>) {
        let name = self
            .theme_override
            .as_deref()
            .unwrap_or_else(|| self.config.theme.name_for(appearance));
        let (theme, fell_back) = self.registry.get_or_default(name, appearance);
        let notice = fell_back.then(|| {
            format!("theme: no installed theme has the configured name; using {}", theme.name)
        });
        (theme, notice)
    }

    /// The font settings for panes.
    #[must_use]
    pub fn font(&self) -> FontSpec {
        let f = &self.config.font;
        FontSpec { family: f.family.clone(), size: f.size, line_height: f.line_height }
    }
}

/// One line for the banner: file name, line, key and what is wrong, so the
/// key stays visible in a narrow window. Values never appear.
#[must_use]
pub fn describe_error(e: &ConfigError) -> String {
    let short = ConfigError {
        path: e.path.as_deref().and_then(Path::file_name).map(PathBuf::from),
        ..e.clone()
    };
    format!("{short} (config not applied, keeping the previous settings)")
}

/// Banner line for a keybinding problem found after parsing: only the
/// keybindings fall back to the defaults, everything else in the file applies.
#[must_use]
pub fn describe_keymap_error(e: &ConfigError) -> String {
    let short = ConfigError {
        path: e.path.as_deref().and_then(Path::file_name).map(PathBuf::from),
        ..e.clone()
    };
    format!("{short} (using the default keybindings; the rest of the config is applied)")
}

/// Font settings for a pane.
#[derive(Clone, Debug, PartialEq)]
pub struct FontSpec {
    /// Family, or `monospace` for the platform's monospace font.
    pub family: String,
    /// Size in points.
    pub size: f32,
    /// Line height as a multiple of the size.
    pub line_height: f32,
}

/// Smallest and largest font size the font size actions reach.
pub const FONT_SIZE_RANGE: (f32, f32) = (4.0, 128.0);

/// Apply a font size step (in points) and keep it in range.
#[must_use]
pub fn step_font_size(base: f32, delta: f32) -> f32 {
    (base + delta).clamp(FONT_SIZE_RANGE.0, FONT_SIZE_RANGE.1)
}

/// The engine palette for a theme. The config crate hands out plain data so
/// it does not depend on the engine's own type; this is the conversion.
#[must_use]
pub fn engine_palette(p: &TerminalPalette) -> Palette {
    Palette {
        ansi: p.ansi,
        foreground: p.foreground,
        background: p.background,
        cursor: p.cursor,
        selection_background: p.selection_bg,
        selection_foreground: Some(p.selection_fg),
    }
}

/// The configured cursor shape.
#[must_use]
pub fn cursor_shape(style: CursorStyle) -> CursorShape {
    match style {
        CursorStyle::Block => CursorShape::Block,
        CursorStyle::Bar => CursorShape::Bar,
        CursorStyle::Underline => CursorShape::Underline,
    }
}

/// The shape to draw. The engine reports a plain block both when a program
/// asked for one and when nothing asked at all, so a block is taken to mean
/// "the default" and the configured style is drawn. A bar, underline or
/// hollow block from the program wins.
#[must_use]
pub fn effective_cursor(reported: CursorShape, configured: CursorShape) -> CursorShape {
    match reported {
        CursorShape::Block => configured,
        other => other,
    }
}

/// Map GPUI's window appearance (as a dark flag) to a theme appearance.
#[must_use]
pub fn appearance(dark: bool) -> Appearance {
    if dark { Appearance::Dark } else { Appearance::Light }
}

/// Where a new shell starts. `inherited` is the focused pane's directory,
/// used for `working_directory = "inherit"`. A restored pane passes its
/// saved directory as `restored`: it starts there while the directory
/// exists, else in home (`persist::resolve_cwd`), never somewhere the
/// config picked for new panes.
#[must_use]
pub fn start_directory(
    setting: &WorkingDirectory,
    inherited: Option<&Path>,
    restored: Option<&Path>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(r) = restored {
        return match home {
            Some(h) => Some(halyard_workspace::persist::resolve_cwd(Some(r), h)),
            None => r.is_absolute().then(|| r.to_path_buf()).filter(|p| p.is_dir()),
        };
    }
    match setting {
        WorkingDirectory::Home => None,
        WorkingDirectory::Inherit => inherited.filter(|p| p.is_dir()).map(Path::to_path_buf),
        WorkingDirectory::Path(p) => {
            let expanded = match p.strip_prefix("~") {
                Ok(rest) => home.map(|h| h.join(rest))?,
                Err(_) => p.clone(),
            };
            expanded.is_dir().then_some(expanded)
        }
    }
}

/// The configured shell as a program path and arguments, or `None` for the
/// login shell. A bare name is looked up on `PATH` by the PTY layer.
#[must_use]
pub fn shell_command(config: &Config) -> Option<(PathBuf, Vec<String>)> {
    config.terminal.shell.as_ref().map(|s| (PathBuf::from(&s.program), s.args.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use halyard_config::ConfigWatcher;
    use std::time::Duration;

    fn write(dir: &Path, text: &str) -> PathBuf {
        let p = dir.join("config.toml");
        std::fs::write(&p, text).unwrap();
        p
    }

    #[test]
    fn load_applies_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(
            dir.path(),
            "[font]\nsize = 17\nline_height = 1.5\n[terminal]\ncursor_style = \"bar\"\n[theme]\nname = \"Kelp\"\n",
        );
        let s = Settings::load(Some(&p));
        assert!(s.notices.is_empty(), "{:?}", s.notices);
        assert_eq!(s.font().size, 17.0);
        assert_eq!(s.font().line_height, 1.5);
        assert_eq!(cursor_shape(s.config.terminal.cursor_style), CursorShape::Bar);
        assert_eq!(s.theme(Appearance::Light).0.name, "Kelp");
    }

    #[test]
    fn a_broken_file_gives_defaults_and_a_notice_never_a_value() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "[font]\nsize = \"sk-secret-value\"\n");
        let s = Settings::load(Some(&p));
        assert_eq!(s.config, Config::default());
        assert_eq!(s.notices.len(), 1, "{:?}", s.notices);
        assert!(s.notices[0].contains("font.size"), "{:?}", s.notices);
        assert!(!s.notices[0].contains("sk-secret"), "{:?}", s.notices);
        assert!(s.notices[0].starts_with("config.toml:2: "), "short path: {:?}", s.notices);
    }

    #[test]
    fn a_broken_reload_keeps_the_previous_settings() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "[font]\nsize = 20\n");
        let mut s = Settings::load(Some(&p));
        assert_eq!(s.font().size, 20.0);
        let gen0 = s.generation;
        s.apply(Config::parse("[font]\nsize = 999\n"));
        assert_eq!(s.font().size, 20.0, "kept the last good config");
        assert!(s.notices[0].contains("keeping the previous settings"), "{:?}", s.notices);
        assert!(s.notices[0].contains("font.size"), "{:?}", s.notices);
        assert!(s.generation > gen0);
        s.apply(Config::parse("[font]\nsize = 11\n"));
        assert_eq!(s.font().size, 11.0);
        assert!(s.notices.is_empty());
    }

    #[test]
    fn a_bad_keybinding_keeps_the_rest_of_the_config() {
        // `Config::parse` rejects bad chords, so build one by hand, as an
        // embedder or a future loader could.
        let mut config = Config::default();
        config.font.size = 21.0;
        config.keybindings.insert("cmd+".into(), "new_tab".into());
        let mut s = Settings::defaults();
        s.apply(Ok(Loaded { config, warnings: Vec::new() }));
        assert_eq!(s.font().size, 21.0, "the rest is applied");
        assert_eq!(s.keymap, Keymap::defaults(Platform::current()));
        assert_eq!(s.notices.len(), 1, "{:?}", s.notices);
        assert!(s.notices[0].contains("the rest of the config is applied"), "{:?}", s.notices);
        assert!(!s.notices[0].contains("not applied"), "{:?}", s.notices);
    }

    #[test]
    fn unknown_keys_become_notices() {
        let mut s = Settings::defaults();
        s.apply(Config::parse("[font]\nsise = 3\n"));
        assert!(s.notices.iter().any(|n| n.contains("font.sise")), "{:?}", s.notices);
    }

    #[test]
    fn unknown_theme_falls_back_with_a_notice() {
        let mut s = Settings::defaults();
        s.apply(Config::parse("[theme]\nname = \"No Such Theme\"\n"));
        let (t, n) = s.theme(Appearance::Dark);
        assert_eq!(t.name, "Halyard Night");
        assert!(n.is_some());
        assert!(s.notices.iter().any(|x| x.contains("using Halyard Night")), "{:?}", s.notices);
        assert!(!s.notices.iter().any(|x| x.contains("No Such Theme")), "no value echoed");
    }

    #[test]
    fn light_and_dark_follow_the_appearance() {
        let mut s = Settings::defaults();
        s.apply(Config::parse("[theme]\nlight = \"Paper Chart\"\ndark = \"Kelp\"\n"));
        assert_eq!(s.theme(appearance(true)).0.name, "Kelp");
        assert_eq!(s.theme(appearance(false)).0.name, "Paper Chart");
    }

    #[test]
    fn override_holds_until_the_theme_section_changes() {
        let mut s = Settings::defaults();
        s.theme_override = Some("Signal Flare".into());
        s.apply(Config::parse("[font]\nsize = 15\n"));
        assert_eq!(s.theme(Appearance::Dark).0.name, "Signal Flare");
        s.apply(Config::parse("[theme]\nname = \"Kelp\"\n"));
        assert_eq!(s.theme_override, None);
        assert_eq!(s.theme(Appearance::Dark).0.name, "Kelp");
    }

    #[test]
    fn user_themes_load_from_the_themes_folder() {
        let dir = tempfile::tempdir().unwrap();
        let themes = dir.path().join("themes");
        std::fs::create_dir(&themes).unwrap();
        let mine = ThemeRegistry::bundled().get("Kelp").unwrap().clone();
        let mut mine = mine;
        mine.name = "Mine".into();
        std::fs::write(themes.join("mine.toml"), mine.to_toml()).unwrap();
        std::fs::write(themes.join("bad.toml"), "nope = [").unwrap();
        let p = write(dir.path(), "[theme]\nname = \"Mine\"\n");
        let s = Settings::load(Some(&p));
        assert_eq!(s.theme(Appearance::Dark).0.name, "Mine");
        assert!(s.notices.iter().any(|n| n.contains("bad.toml")), "{:?}", s.notices);
    }

    #[test]
    fn keybinding_overrides_reach_the_keymap() {
        let mut s = Settings::defaults();
        s.apply(Config::parse("[keybindings]\n\"primary+k\" = \"command_palette\"\n"));
        let chord = halyard_config::KeyChord::parse("primary+k", Platform::current()).unwrap();
        assert_eq!(s.keymap.action_for(&chord), Some(halyard_config::Action::CommandPalette));
    }

    #[test]
    fn theme_palette_reaches_the_engine_palette() {
        let t = ThemeRegistry::bundled().get("Signal Flare").unwrap().clone();
        let p = engine_palette(&t.palette());
        assert_eq!(p.background, t.terminal.background);
        assert_eq!(p.foreground, t.terminal.foreground);
        assert_eq!(p.ansi, t.terminal.ansi);
        assert_eq!(p.selection_background, t.terminal.selection_bg);
        assert_eq!(p.selection_foreground, Some(t.terminal.selection_fg));
        // And the engine draws with it.
        let mut e = halyard_term::Engine::new(10, 2, 0, &p).unwrap();
        e.write(b"\x1b[31mr");
        let f = e.frame().unwrap();
        assert_eq!(f.background, t.terminal.background);
        assert_eq!(f.cell(0, 0).unwrap().fg, t.terminal.ansi[1]);
    }

    #[test]
    fn configured_cursor_replaces_only_the_default_block() {
        assert_eq!(effective_cursor(CursorShape::Block, CursorShape::Bar), CursorShape::Bar);
        assert_eq!(
            effective_cursor(CursorShape::Underline, CursorShape::Bar),
            CursorShape::Underline
        );
        assert_eq!(effective_cursor(CursorShape::Block, CursorShape::Block), CursorShape::Block);
    }

    #[test]
    fn font_steps_stay_in_range() {
        assert_eq!(step_font_size(13.0, 1.0), 14.0);
        assert_eq!(step_font_size(4.5, -2.0), 4.0);
        assert_eq!(step_font_size(127.0, 5.0), 128.0);
    }

    #[test]
    fn start_directory_rules() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let gone = d.join("gone");
        assert_eq!(start_directory(&WorkingDirectory::Home, Some(d), None, Some(d)), None);
        assert_eq!(
            start_directory(&WorkingDirectory::Inherit, Some(d), None, None),
            Some(d.to_path_buf())
        );
        assert_eq!(start_directory(&WorkingDirectory::Inherit, Some(&gone), None, None), None);
        std::fs::create_dir(d.join("proj")).unwrap();
        assert_eq!(
            start_directory(&WorkingDirectory::Path("~/proj".into()), None, None, Some(d)),
            Some(d.join("proj"))
        );
        // A saved directory wins while it exists, and is ignored once gone.
        assert_eq!(
            start_directory(&WorkingDirectory::Home, None, Some(&d.join("proj")), None),
            Some(d.join("proj"))
        );
        assert_eq!(start_directory(&WorkingDirectory::Home, None, Some(&gone), None), None);
        // With a home directory, a vanished saved directory restores to home
        // (persist::resolve_cwd), even when new panes would start elsewhere.
        let elsewhere = WorkingDirectory::Path(d.join("proj"));
        assert_eq!(start_directory(&elsewhere, None, Some(&gone), Some(d)), Some(d.to_path_buf()));
        assert_eq!(
            start_directory(&elsewhere, None, Some(&d.join("proj")), Some(d)),
            Some(d.join("proj"))
        );
        assert_eq!(
            start_directory(&WorkingDirectory::Home, None, Some(Path::new("relative")), None),
            None
        );
    }

    #[test]
    fn shell_setting_becomes_a_command() {
        let c = Config::parse("[terminal]\nshell = { program = \"/bin/sh\", args = [\"-i\"] }\n")
            .unwrap()
            .config;
        assert_eq!(shell_command(&c), Some((PathBuf::from("/bin/sh"), vec!["-i".to_owned()])));
        assert_eq!(shell_command(&Config::default()), None);
    }

    /// The watcher plus `apply` is the live reload path the app runs.
    #[test]
    fn watcher_reload_reaches_settings() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "[font]\nsize = 12\n");
        let mut s = Settings::load(Some(&p));
        let w = ConfigWatcher::with_poll_interval(
            &p,
            Duration::from_millis(50),
            Duration::from_millis(50),
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(60));
        std::fs::write(&p, "[font]\nsize = 19\n[theme]\nname = \"Harbor Fog\"\n").unwrap();
        let ev = w.recv_timeout(Duration::from_secs(5)).expect("a reload event");
        s.apply(ev);
        assert_eq!(s.font().size, 19.0);
        assert_eq!(s.theme(Appearance::Dark).0.name, "Harbor Fog");
    }
}
