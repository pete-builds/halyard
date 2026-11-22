use std::ffi::OsString;
use std::path::PathBuf;

use halyard_config::{
    Action, Config, CursorStyle, Decorations, Platform, ProviderKind, WorkingDirectory,
    config_path_with, default_config_toml, themes_dir_for,
};

fn err(src: &str) -> halyard_config::ConfigError {
    Config::parse(src).expect_err("expected an error")
}

#[test]
fn default_config_file_parses_to_exactly_the_default_impl() {
    let loaded = Config::parse(default_config_toml()).expect("default-config.toml must parse");
    assert_eq!(loaded.config, Config::default());
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
}

#[test]
fn default_config_control_a_changed_value_is_detected() {
    // Control: the equality above can fail. Change one value and it must differ.
    let edited = default_config_toml().replace("size = 13.0", "size = 14.0");
    assert_ne!(Config::parse(&edited).unwrap().config, Config::default());
}

#[test]
fn default_config_documents_every_key() {
    // Every key the validator knows must appear in the template, active or commented.
    for key in [
        "family",
        "size",
        "line_height",
        "bold_is_bright",
        "padding_x",
        "padding_y",
        "opacity",
        "blur",
        "decorations",
        "initial_cols",
        "initial_rows",
        "scrollback_lines",
        "shell",
        "working_directory",
        "cursor_style",
        "cursor_blink",
        "copy_on_select",
        "confirm_close_running",
        "light",
        "dark",
        "name",
        "enabled",
        "providers",
        "api_key_env",
        "base_url",
        "kind",
        "model",
        "notify_on_waiting",
        "detect",
    ] {
        assert!(default_config_toml().contains(key), "template lacks `{key}`");
    }
    for a in Action::ALL {
        assert!(default_config_toml().contains(a.name()), "template lacks action {}", a.name());
    }
}

#[test]
fn empty_file_is_defaults() {
    assert_eq!(Config::parse("").unwrap().config, Config::default());
}

#[test]
fn missing_file_is_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let loaded = Config::load(&dir.path().join("nope.toml")).unwrap();
    assert_eq!(loaded.config, Config::default());
    assert!(loaded.warnings.is_empty());
}

#[test]
fn full_config_round_trips_values() {
    let src = r#"
[font]
family = "JetBrains Mono"
size = 15
line_height = 1.4
bold_is_bright = true

[window]
padding_x = 2
opacity = 0.9
blur = true
decorations = "transparent_titlebar"
initial_cols = 120
initial_rows = 40

[terminal]
scrollback_lines = 50000
working_directory = "~/code"
cursor_style = "bar"
cursor_blink = false
copy_on_select = true
confirm_close_running = false
shell = { program = "/bin/fish", args = ["-l"] }

[theme]
name = "Kelp"

[agents]
notify_on_waiting = false
detect = ["aider", "goose"]

[ai]
enabled = true

[[ai.providers]]
name = "local"
kind = "ollama"
base_url = "http://localhost:11434"
model = "llama3.1"

[[ai.providers]]
name = "claude"
kind = "anthropic"
model = "claude-sonnet-4-5"
api_key_env = "ANTHROPIC_API_KEY"
"#;
    let c = Config::parse(src).unwrap().config;
    assert_eq!(c.font.family, "JetBrains Mono");
    assert!((c.font.size - 15.0).abs() < f32::EPSILON);
    assert!(c.font.bold_is_bright);
    assert_eq!(c.window.decorations, Decorations::TransparentTitlebar);
    assert_eq!(c.window.initial_cols, 120);
    assert_eq!(c.terminal.cursor_style, CursorStyle::Bar);
    assert_eq!(c.terminal.working_directory, WorkingDirectory::Path(PathBuf::from("~/code")));
    let shell = c.terminal.shell.unwrap();
    assert_eq!(
        (shell.program.as_str(), shell.args.as_slice()),
        ("/bin/fish", ["-l".to_string()].as_slice())
    );
    assert_eq!(c.theme.name_for(halyard_config::Appearance::Light), "Kelp");
    assert_eq!(c.agents.detect, ["aider", "goose"]);
    assert!(c.ai.enabled);
    assert_eq!(c.ai.providers.len(), 2);
    assert_eq!(c.ai.providers[0].kind, ProviderKind::Ollama);
    assert_eq!(c.ai.providers[1].api_key_env.as_deref(), Some("ANTHROPIC_API_KEY"));
}

#[test]
fn theme_light_dark_pair_follows_appearance() {
    let c = Config::parse("[theme]\nlight = \"Paper Chart\"\ndark = \"Signal Flare\"\n")
        .unwrap()
        .config;
    assert_eq!(c.theme.name_for(halyard_config::Appearance::Light), "Paper Chart");
    assert_eq!(c.theme.name_for(halyard_config::Appearance::Dark), "Signal Flare");
}

#[test]
fn theme_name_with_light_or_dark_is_an_error() {
    let e = err("[theme]\nname = \"Kelp\"\ndark = \"Halyard Night\"\n");
    assert_eq!(e.key.as_deref(), Some("theme.name"));
    assert_eq!(e.line, Some(2));
    assert!(e.message.contains("cannot be combined"), "{e}");
}

#[test]
fn unknown_keys_warn_with_key_and_line() {
    let src =
        "bogus = 1\n[font]\nsize = 12\nfamliy = \"x\"\n[mystery]\nx = 1\n[terminal.shell_extra]\n";
    let loaded = Config::parse(src).unwrap();
    let keys: Vec<(&str, Option<usize>)> =
        loaded.warnings.iter().map(|w| (w.key.as_str(), w.line)).collect();
    assert!(keys.contains(&("bogus", Some(1))), "{keys:?}");
    assert!(keys.contains(&("font.famliy", Some(4))), "{keys:?}");
    assert!(keys.contains(&("mystery", Some(5))), "{keys:?}");
    let typo = loaded.warnings.iter().find(|w| w.key == "font.famliy").unwrap();
    assert!(typo.message.contains("did you mean `family`"), "{typo}");
    assert_eq!(loaded.config.font.size as u32, 12, "known keys still apply");
}

#[test]
fn unknown_provider_and_shell_keys_warn() {
    let src = "[terminal]\nshell = { program = \"sh\", colour = 1 }\n[[ai.providers]]\nname = \"a\"\nkind = \"anthropic\"\nmodel = \"m\"\nflavor = \"x\"\n";
    let w = Config::parse(src).unwrap().warnings;
    assert!(w.iter().any(|w| w.key == "terminal.shell.colour" && w.line == Some(2)), "{w:?}");
    assert!(w.iter().any(|w| w.key == "ai.providers[0].flavor" && w.line == Some(7)), "{w:?}");
}

#[test]
fn invalid_type_names_key_and_line() {
    let e = err("[font]\nfamily = \"x\"\nsize = \"big\"\n");
    assert_eq!(e.key.as_deref(), Some("font.size"));
    assert_eq!(e.line, Some(3));
    assert!(e.to_string().contains("font.size"), "{e}");
}

#[test]
fn invalid_enum_value_names_key_and_line() {
    let e = err("[terminal]\ncursor_style = \"sparkle\"\n");
    assert_eq!(e.key.as_deref(), Some("terminal.cursor_style"));
    assert_eq!(e.line, Some(2));
    assert!(e.message.contains("sparkle") && e.message.contains("block"), "{e}");
}

#[test]
fn invalid_working_directory_names_key() {
    let e = err("[terminal]\nworking_directory = \"relative/dir\"\n");
    assert_eq!(e.key.as_deref(), Some("terminal.working_directory"));
    assert!(e.message.contains("absolute"), "{e}");
}

#[test]
fn out_of_range_values_name_key_and_line() {
    for (src, key, line) in [
        ("[font]\nsize = 3\n", "font.size", 2),
        ("[font]\nline_height = 9.0\n", "font.line_height", 2),
        ("[window]\nopacity = 1.5\n", "window.opacity", 2),
        ("\n[window]\n\ninitial_rows = 1\n", "window.initial_rows", 4),
        ("[terminal]\nscrollback_lines = 5000000\n", "terminal.scrollback_lines", 2),
    ] {
        let e = err(src);
        assert_eq!((e.key.as_deref(), e.line), (Some(key), Some(line)), "{src:?}: {e}");
        assert!(e.message.contains("out of range"), "{e}");
    }
}

#[test]
fn toml_syntax_error_reports_line() {
    let e = err("[font]\nsize = = 3\n");
    assert_eq!(e.line, Some(2));
}

#[test]
fn error_display_includes_path_when_loaded_from_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("config.toml");
    std::fs::write(&file, "[window]\nopacity = 2\n").unwrap();
    let e = Config::load(&file).unwrap_err();
    let shown = e.to_string();
    assert!(shown.contains("config.toml:2"), "{shown}");
    assert!(shown.contains("window.opacity"), "{shown}");
}

#[test]
fn api_key_in_config_is_rejected_with_guidance() {
    let src = "[[ai.providers]]\nname = \"a\"\nkind = \"anthropic\"\nmodel = \"m\"\napi_key = \"sk-ant-abc\"\n";
    let e = err(src);
    assert_eq!(e.key.as_deref(), Some("ai.providers[0].api_key"));
    assert_eq!(e.line, Some(5));
    assert!(e.message.contains("api_key_env") && e.message.contains("environment"), "{e}");
    assert!(!e.to_string().contains("sk-ant-abc"), "the secret must not be echoed back");
}

#[test]
fn other_secret_like_keys_are_rejected_too() {
    for k in ["token", "secret", "password", "key", "apikey"] {
        let src = format!(
            "[[ai.providers]]\nname = \"a\"\nkind = \"anthropic\"\nmodel = \"m\"\n{k} = \"x\"\n"
        );
        assert!(err(&src).message.contains("never stores API keys"), "{k}");
    }
}

#[test]
fn api_key_env_must_be_a_variable_name_not_a_key() {
    let src = "[[ai.providers]]\nname = \"a\"\nkind = \"anthropic\"\nmodel = \"m\"\napi_key_env = \"sk-ant-api03-abc\"\n";
    let e = err(src);
    assert_eq!(e.key.as_deref(), Some("ai.providers[0].api_key_env"));
    assert!(e.message.contains("NAME"), "{e}");
    assert!(!e.to_string().contains("sk-ant"), "the pasted key must not be echoed back");
}

#[test]
fn provider_validation() {
    let base = "[[ai.providers]]\nname = \"a\"\nkind = \"ollama\"\nmodel = \"m\"\n";
    let e = err(base);
    assert_eq!(e.key.as_deref(), Some("ai.providers[0].base_url"));
    let e = err(&format!("{base}base_url = \"localhost:11434\"\n"));
    assert!(e.message.contains("http"), "{e}");
    let dup = format!("{base}base_url = \"http://x\"\n{base}base_url = \"http://y\"\n");
    let e = err(&dup);
    assert_eq!(e.key.as_deref(), Some("ai.providers[1].name"));
    assert_eq!(e.line, Some(7));
    let e = err("[[ai.providers]]\nname = \"a\"\nkind = \"carrier-pigeon\"\nmodel = \"m\"\n");
    assert!(e.message.contains("carrier-pigeon"), "{e}");
    let ok = Config::parse("[[ai.providers]]\nname = \"c\"\nkind = \"anthropic\"\nmodel = \"m\"\n")
        .unwrap();
    assert_eq!(ok.config.ai.providers[0].base_url, None);
}

#[test]
fn ai_is_off_by_default() {
    assert!(!Config::default().ai.enabled);
}

#[test]
fn keybindings_override_add_and_unbind() {
    let src = "[keybindings]\n\"cmd+k\" = \"command_palette\"\n\"cmd+q\" = \"none\"\n\"cmd+t\" = \"new_window\"\n";
    let c = Config::parse(src).unwrap().config;
    let map = c.keymap(Platform::Mac).unwrap();
    let chord = |s: &str| halyard_config::KeyChord::parse(s, Platform::Mac).unwrap();
    assert_eq!(map.action_for(&chord("cmd+k")), Some(Action::CommandPalette));
    assert_eq!(map.action_for(&chord("cmd+q")), None);
    assert_eq!(map.action_for(&chord("cmd+t")), Some(Action::NewWindow));
    assert_eq!(map.action_for(&chord("cmd+w")), Some(Action::ClosePane), "untouched defaults stay");
}

#[test]
fn primary_in_keybindings_resolves_per_platform() {
    let c = Config::parse("[keybindings]\n\"primary+k\" = \"find\"\n").unwrap().config;
    let mac = c.keymap(Platform::Mac).unwrap();
    let lin = c.keymap(Platform::Linux).unwrap();
    let k = |s, p| halyard_config::KeyChord::parse(s, p).unwrap();
    assert_eq!(mac.action_for(&k("cmd+k", Platform::Mac)), Some(Action::Find));
    assert_eq!(lin.action_for(&k("ctrl+shift+k", Platform::Linux)), Some(Action::Find));
}

#[test]
fn bad_keybinding_chord_names_key_and_line() {
    let e = err("[keybindings]\n\"cmd+shift\" = \"new_tab\"\n");
    assert_eq!(e.key.as_deref(), Some("keybindings.cmd+shift"));
    assert_eq!(e.line, Some(2));
    assert!(e.message.contains("no key"), "{e}");
}

#[test]
fn bad_keybinding_action_lists_valid_actions() {
    let e = err("[keybindings]\n\"cmd+k\" = \"teleport\"\n");
    assert_eq!(e.key.as_deref(), Some("keybindings.cmd+k"));
    assert!(e.message.contains("teleport") && e.message.contains("new_tab"), "{e}");
}

#[test]
fn duplicate_chords_after_primary_expansion_are_rejected() {
    // Same chord on Linux (ctrl+shift+k) but distinct strings.
    let e = err("[keybindings]\n\"primary+k\" = \"find\"\n\"ctrl+shift+k\" = \"copy\"\n");
    assert!(e.message.contains("same chord"), "{e}");
}

#[test]
fn config_path_precedence() {
    let env = |pairs: &'static [(&'static str, &'static str)]| {
        move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| OsString::from(v))
    };
    assert_eq!(
        config_path_with(env(&[
            ("HALYARD_CONFIG", "/x/c.toml"),
            ("XDG_CONFIG_HOME", "/xdg"),
            ("HOME", "/h")
        ])),
        Some(PathBuf::from("/x/c.toml"))
    );
    assert_eq!(
        config_path_with(env(&[("XDG_CONFIG_HOME", "/xdg"), ("HOME", "/h")])),
        Some(PathBuf::from("/xdg/halyard/config.toml"))
    );
    assert_eq!(
        config_path_with(env(&[("HOME", "/h")])),
        Some(PathBuf::from("/h/.config/halyard/config.toml"))
    );
    // Relative or empty XDG_CONFIG_HOME is ignored.
    assert_eq!(
        config_path_with(env(&[("XDG_CONFIG_HOME", "rel"), ("HOME", "/h")])),
        Some(PathBuf::from("/h/.config/halyard/config.toml"))
    );
    assert_eq!(
        config_path_with(env(&[("HALYARD_CONFIG", ""), ("XDG_CONFIG_HOME", ""), ("HOME", "/h")])),
        Some(PathBuf::from("/h/.config/halyard/config.toml"))
    );
    assert_eq!(config_path_with(env(&[])), None);
}

#[test]
fn themes_dir_sits_next_to_config() {
    assert_eq!(
        themes_dir_for(&PathBuf::from("/h/.config/halyard/config.toml")),
        PathBuf::from("/h/.config/halyard/themes")
    );
}
