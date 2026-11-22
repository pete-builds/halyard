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
    // Under [ai] the unknown key's name is not shown, only where it is.
    assert!(w.iter().any(|w| w.key == "ai.providers[0]" && w.line == Some(7)), "{w:?}");
    assert!(w.iter().all(|w| !w.to_string().contains("flavor")), "{w:?}");
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
    assert!(e.message.contains("block") && !e.to_string().contains("sparkle"), "{e}");
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
    // The section and line, not the key's name: an unknown name under [ai] may be a secret.
    assert_eq!(e.key.as_deref(), Some("ai.providers[0]"));
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

/// Parse `src`, assert it fails or warns, and assert `secret` is in none of the text.
fn assert_secret_absent(src: &str, secret: &str) {
    match Config::parse(src) {
        Err(e) => {
            assert!(!e.to_string().contains(secret), "error echoed the secret: {e}");
            assert!(!format!("{e:?}").contains(secret), "error debug echoed the secret: {e:?}");
        }
        Ok(l) => {
            for w in &l.warnings {
                assert!(!w.to_string().contains(secret), "warning echoed the secret: {w}");
            }
            panic!("expected an error for {src}");
        }
    }
}

const SECRET: &str = "sk-ant-api03-SUPERSECRET";
const PROV: &str = "[[ai.providers]]\nname = \"a\"\nmodel = \"m\"\n";

#[test]
fn secret_in_kind_is_not_echoed() {
    assert_secret_absent(&format!("{PROV}kind = \"{SECRET}\"\n"), SECRET);
}

#[test]
fn secret_as_integer_api_key_env_is_not_echoed() {
    let src = format!("{PROV}kind = \"anthropic\"\napi_key_env = 12345678\n");
    assert_secret_absent(&src, "12345678");
}

#[test]
fn secret_in_base_url_is_not_echoed() {
    let src = format!("{PROV}kind = \"ollama\"\nbase_url = \"{SECRET}\"\n");
    assert_secret_absent(&src, SECRET);
}

#[test]
fn base_url_with_userinfo_is_rejected_without_echo() {
    let src =
        format!("{PROV}kind = \"ollama\"\nbase_url = \"https://u:{SECRET}@host.example/v1\"\n");
    let e = err(&src);
    assert_eq!(e.key.as_deref(), Some("ai.providers[0].base_url"));
    assert!(e.message.contains("credentials"), "{e}");
    assert_secret_absent(&src, SECRET);
    assert_secret_absent(&src, "u:");
    // Control: the same URL without userinfo is fine, and an `@` in the path is not userinfo.
    let ok = format!("{PROV}kind = \"ollama\"\nbase_url = \"https://host.example/@v1\"\n");
    assert!(Config::parse(&ok).is_ok());
}

#[test]
fn secret_in_duplicate_provider_name_is_not_echoed() {
    let one =
        format!("[[ai.providers]]\nname = \"{SECRET}\"\nkind = \"anthropic\"\nmodel = \"m\"\n");
    assert_secret_absent(&format!("{one}{one}"), SECRET);
}

#[test]
fn secret_in_wrongly_typed_ai_value_is_not_echoed() {
    assert_secret_absent(&format!("[ai]\nenabled = \"{SECRET}\"\n"), SECRET);
    assert_secret_absent(
        &format!("{PROV}kind = \"anthropic\"\nmodel = 7\nname = \"{SECRET}\"\n"),
        SECRET,
    );
}

#[test]
fn secret_in_ai_syntax_error_is_not_echoed() {
    assert_secret_absent(&format!("{PROV}kind = \"anthropic\"\napi_key_env = {SECRET}\n"), SECRET);
}

#[test]
fn secret_like_key_names_are_rejected_case_insensitively_anywhere_under_ai() {
    for k in ["API_KEY", "Authorization", "x-api-key", "Bearer_Token", "DB_PASSWD", "ApiSecret"] {
        let provider = format!("{PROV}kind = \"anthropic\"\n\"{k}\" = \"{SECRET}\"\n");
        let e = err(&provider);
        assert!(e.message.contains("never stores API keys"), "{k}: {e}");
        assert_secret_absent(&provider, SECRET);
        // Directly under [ai], not only in a provider.
        let top = format!("[ai]\n\"{k}\" = \"{SECRET}\"\n");
        let e = err(&top);
        assert_eq!(e.key.as_deref(), Some("ai"));
        assert!(!e.to_string().contains(k), "key name {k} must not be echoed: {e}");
        assert!(e.message.contains("never stores API keys"), "{k}: {e}");
        assert_secret_absent(&top, SECRET);
        // Nested in a table inside a provider, e.g. headers.
        let nested =
            format!("{PROV}kind = \"anthropic\"\n[ai.providers.headers]\n\"{k}\" = \"{SECRET}\"\n");
        assert!(err(&nested).message.contains("never stores API keys"), "{k}");
        assert_secret_absent(&nested, SECRET);
    }
    // Control: the documented `api_key_env` is not mistaken for a secret.
    assert!(
        Config::parse(&format!("{PROV}kind = \"anthropic\"\napi_key_env = \"MY_KEY\"\n")).is_ok()
    );
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
    assert_eq!(e.key.as_deref(), Some("ai.providers[0].kind"));
    assert!(!e.to_string().contains("carrier-pigeon"), "[ai] values are never echoed: {e}");
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
    assert!(e.message.contains("new_tab") && !e.to_string().contains("teleport"), "{e}");
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

// ---------------------------------------------------------------------------
// No error or warning ever contains a value from the file
// ---------------------------------------------------------------------------

const SENT: &str = "ZQXSENT7731";

/// Parse `src`. Every error and warning, in Display and Debug form, must be free of
/// [`SENT`]. Returns whether it was an error.
fn leak_check(src: &str) -> bool {
    match Config::parse(src) {
        Err(e) => {
            let shown = format!("{e} || {e:?}");
            assert!(!shown.contains(SENT), "error leaked for {src:?}: {shown}");
            true
        }
        Ok(l) => {
            for w in &l.warnings {
                let shown = format!("{w} || {w:?}");
                assert!(!shown.contains(SENT), "warning leaked for {src:?}: {shown}");
            }
            false
        }
    }
}

#[test]
fn quoted_ai_headers_do_not_hide_a_value_from_redaction() {
    for header in ["[\"ai\"]", "['ai']", "[ \"ai\" ]", "[ai]"] {
        assert!(leak_check(&format!("{header}\nenabled = \"{SENT}\"\n")), "{header}");
    }
    for header in ["[[\"ai\".providers]]", "[['ai'.providers]]", "[[ai.\"providers\"]]"] {
        let src = format!("{header}\nname = \"a\"\nmodel = \"m\"\nkind = \"{SENT}\"\n");
        assert!(leak_check(&src), "{header}");
    }
}

#[test]
fn dotted_ai_keys_do_not_hide_a_value_from_redaction() {
    assert!(leak_check(&format!("\"ai\".enabled = \"{SENT}\"\n")));
    assert!(leak_check(&format!("ai.enabled = \"{SENT}\"\n")));
    assert!(leak_check(&format!("ai = {{ enabled = \"{SENT}\" }}\n")));
    assert!(leak_check(&format!(
        "[ai]\nproviders = [{{ name = \"a\", model = \"m\", kind = \"{SENT}\" }}]\n"
    )));
}

#[test]
fn a_header_lookalike_in_a_multiline_string_does_not_move_the_section() {
    // Line scanning saw `[font]` inside the string and blamed `font.enabled`.
    let src = format!("[ai]\nnote = \"\"\"\n[font]\n\"\"\"\nenabled = \"{SENT}\"\n");
    assert!(leak_check(&src));
    let src = format!("[ai]\nnote = \"\"\"\n[font]\nenabled = 1\n\"\"\"\nenabled = \"{SENT}\"\n");
    assert!(leak_check(&src));
    // The reported location is structural: the real `enabled`, not the one in the string.
    let e = err(&src);
    assert_eq!(e.key.as_deref(), Some("ai.enabled"), "{e}");
    assert_eq!(e.line, Some(6), "{e}");
}

#[test]
fn secret_names_are_found_through_quoting_dotted_keys_and_multiline_strings() {
    for src in [
        format!("[\"ai\"]\n\"API_KEY\" = \"{SENT}\"\n"),
        format!("\"ai\".\"x-api-key\" = \"{SENT}\"\n"),
        format!("ai.Authorization = \"{SENT}\"\n"),
        format!(
            "[[ai.providers]]\nname = \"a\"\nkind = \"ollama\"\nmodel = \"m\"\nheaders.Authorization = \"{SENT}\"\n"
        ),
        format!("[ai]\nnote = \"\"\"\n[font]\n\"\"\"\nPASSWORD = \"{SENT}\"\n"),
    ] {
        let e = err(&src);
        assert!(e.message.contains("never stores API keys"), "{src:?}: {e}");
        assert!(leak_check(&src));
    }
}

#[test]
fn unknown_key_names_under_ai_are_not_echoed_but_elsewhere_they_are() {
    let src = format!("[ai]\nenabled = false\n{SENT} = 1\n[font]\n{SENT}_font = 1\n");
    let l = Config::parse(&src).unwrap();
    let ai = l.warnings.iter().find(|w| w.key == "ai").expect("a warning for [ai]");
    assert!(ai.message.contains("unknown key in [ai]") && ai.line == Some(3), "{ai}");
    assert!(!format!("{ai:?}").contains(SENT));
    assert!(
        l.warnings.iter().any(|w| w.key == format!("font.{SENT}_font")),
        "key names outside [ai] may be shown: {:?}",
        l.warnings
    );
}

#[test]
fn no_error_or_warning_ever_echoes_a_value_whatever_the_section() {
    let forms = [
        format!("\"{SENT}\""),
        format!("'{SENT}'"),
        format!("[\"{SENT}\"]"),
        format!("{{ k = \"{SENT}\" }}"),
        format!("\"\"\"\n{SENT}\n[font]\n\"\"\""),
        SENT.to_string(),
        format!("\"{SENT}"),
        format!("[{SENT}"),
        format!("{{ {SENT} = 1 }}"),
    ];
    let keys = [
        "font.family",
        "font.size",
        "font.line_height",
        "font.bold_is_bright",
        "window.padding_x",
        "window.opacity",
        "window.blur",
        "window.decorations",
        "window.initial_cols",
        "terminal.scrollback_lines",
        "terminal.shell",
        "terminal.working_directory",
        "terminal.cursor_style",
        "terminal.cursor_blink",
        "theme.name",
        "theme.light",
        "theme.dark",
        "keybindings.cmd+k",
        "keybindings.cmd+shift+k",
        "agents.notify_on_waiting",
        "agents.detect",
        "ai.enabled",
        "ai.providers",
        "ai.extra",
        "p.name",
        "p.kind",
        "p.base_url",
        "p.model",
        "p.api_key_env",
        "p.extra",
        "p.api_key",
        "top",
    ];
    // How the dotted path is spelled: plain header, quoted header, dotted key at the top.
    let styles = ["header", "quoted", "dotted"];
    let (mut checked, mut errors) = (0, 0);
    for key in keys {
        for form in &forms {
            for style in styles {
                let (section, k) = key.split_once('.').unwrap_or(("", key));
                let kq = if k.contains('+') { format!("\"{k}\"") } else { k.to_string() };
                let text = match (section, style) {
                    ("", _) => format!("{kq} = {form}\n"),
                    ("p", "header") => format!(
                        "[[ai.providers]]\nname = \"a\"\nkind = \"anthropic\"\nmodel = \"m\"\n{kq} = {form}\n"
                    ),
                    ("p", "quoted") => format!(
                        "[[\"ai\".\"providers\"]]\nname = \"a\"\nkind = \"anthropic\"\nmodel = \"m\"\n{kq} = {form}\n"
                    ),
                    ("p", _) => format!(
                        "ai.providers = [{{ name = \"a\", kind = \"anthropic\", model = \"m\", {kq} = {form} }}]\n"
                    ),
                    (sec, "header") => format!("[{sec}]\n{kq} = {form}\n"),
                    (sec, "quoted") => format!("[\"{sec}\"]\n\"{k}\" = {form}\n"),
                    (sec, _) => format!("\"{sec}\".{kq} = {form}\n"),
                };
                checked += 1;
                if leak_check(&text) {
                    errors += 1;
                }
            }
        }
    }
    assert!(checked > 800 && errors > 400, "checked {checked}, errors {errors}: not a real test");
}
