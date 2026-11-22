use std::fs;

use halyard_config::theme::{ANSI_NAMES, DEFAULT_DARK, DEFAULT_LIGHT, mix};
use halyard_config::{
    Appearance, Origin, Theme, ThemeRegistry, check_contrast, contrast_ratio, derive_ui,
    relative_luminance,
};
use halyard_term::frame::Rgb;

#[test]
fn six_original_themes_are_bundled() {
    let reg = ThemeRegistry::bundled();
    assert_eq!(
        reg.names(),
        ["Halyard Night", "Halyard Day", "Harbor Fog", "Kelp", "Signal Flare", "Paper Chart"]
    );
    assert_eq!(reg.default_for(Appearance::Dark).name, DEFAULT_DARK);
    assert_eq!(reg.default_for(Appearance::Light).name, DEFAULT_LIGHT);
    assert_eq!(reg.default_for(Appearance::Light).appearance, Appearance::Light);
    assert!(reg.get("signal flare").unwrap().high_contrast);
    assert!(reg.get("PAPER CHART").unwrap().high_contrast);
    assert!(!reg.get("kelp").unwrap().high_contrast);
}

#[test]
fn bundled_themes_have_unique_palettes() {
    let reg = ThemeRegistry::bundled();
    let all: Vec<&Theme> = reg.entries().map(|e| &e.theme).collect();
    for (i, a) in all.iter().enumerate() {
        for b in &all[i + 1..] {
            assert_ne!(a.terminal.background, b.terminal.background, "{} vs {}", a.name, b.name);
            assert_ne!(a.terminal.ansi, b.terminal.ansi, "{} vs {}", a.name, b.name);
        }
    }
}

#[test]
fn wcag_known_values() {
    let black = Rgb::new(0, 0, 0);
    let white = Rgb::new(255, 255, 255);
    assert!((relative_luminance(black)).abs() < 1e-9);
    assert!((relative_luminance(white) - 1.0).abs() < 1e-9);
    assert!((contrast_ratio(black, white) - 21.0).abs() < 1e-9);
    assert!((contrast_ratio(white, black) - 21.0).abs() < 1e-9, "order must not matter");
    assert!((contrast_ratio(white, white) - 1.0).abs() < 1e-9);
    // #777777 on white is the textbook 4.48:1.
    assert!((contrast_ratio(Rgb::new(0x77, 0x77, 0x77), white) - 4.48).abs() < 0.01);
}

#[test]
fn every_bundled_theme_meets_the_contrast_rule() {
    let reg = ThemeRegistry::bundled();
    for e in reg.entries() {
        let failures = check_contrast(&e.theme);
        assert!(
            failures.is_empty(),
            "{} fails contrast:\n{}",
            e.theme.name,
            failures.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n")
        );
    }
}

fn base_dark() -> Theme {
    ThemeRegistry::bundled().get("Halyard Night").unwrap().clone()
}

#[test]
fn control_a_deliberately_bad_palette_fails_the_contrast_check() {
    let mut t = base_dark();
    // Dark grey red on a near-black background: far below 3.0.
    t.terminal.ansi[1] = Rgb::new(0x20, 0x10, 0x10);
    t.terminal.foreground = Rgb::new(0x30, 0x30, 0x30);
    let f = check_contrast(&t);
    assert!(f.iter().any(|x| x.what.contains("ansi[1]")), "{f:?}");
    assert!(f.iter().any(|x| x.what == "terminal.foreground"), "{f:?}");
}

#[test]
fn contrast_exemptions_are_exactly_black_in_dark_and_white_in_light() {
    // Dark: 0 and 8 may sit on the background; 7 may not.
    let mut t = base_dark();
    t.terminal.ansi[0] = t.terminal.background;
    t.terminal.ansi[8] = t.terminal.background;
    assert!(check_contrast(&t).is_empty());
    t.terminal.ansi[7] = t.terminal.background;
    assert!(check_contrast(&t).iter().any(|x| x.what.contains("ansi[7]")));

    // Light: 7 and 15 are exempt; 0 is not.
    let mut t = ThemeRegistry::bundled().get("Halyard Day").unwrap().clone();
    t.terminal.ansi[7] = t.terminal.background;
    t.terminal.ansi[15] = t.terminal.background;
    assert!(check_contrast(&t).is_empty());
    t.terminal.ansi[0] = t.terminal.background;
    assert!(check_contrast(&t).iter().any(|x| x.what.contains("ansi[0]")));
}

#[test]
fn high_contrast_themes_are_held_to_4_5_on_ansi() {
    let mut t = base_dark();
    // Blue at roughly 3.8:1 passes the normal rule and fails the high-contrast one.
    t.terminal.ansi[4] = Rgb::new(0x4a, 0x78, 0xa8);
    let ratio = contrast_ratio(t.terminal.ansi[4], t.terminal.background);
    assert!((3.0..4.5).contains(&ratio), "test setup: {ratio}");
    assert!(check_contrast(&t).iter().all(|x| !x.what.contains("ansi[4]")));
    t.high_contrast = true;
    assert!(check_contrast(&t).iter().any(|x| x.what.contains("ansi[4]")));
}

#[test]
fn export_round_trips_every_bundled_theme() {
    for e in ThemeRegistry::bundled().entries() {
        let text = e.theme.to_toml();
        let back =
            Theme::from_toml(&text).unwrap_or_else(|err| panic!("{}: {err}\n{text}", e.theme.name));
        assert_eq!(back, e.theme, "{} did not round trip", e.theme.name);
    }
}

#[test]
fn export_control_a_changed_color_breaks_equality() {
    let t = base_dark();
    let text = t.to_toml().replace("#0f1a24", "#0f1a25");
    assert_ne!(Theme::from_toml(&text).unwrap(), t);
}

#[test]
fn minimal_theme_derives_missing_pieces() {
    let src = r##"
name = "Mini"
[terminal]
foreground = "#e0e0e0"
background = "#101010"
ansi = ["#000000", "#cc3333", "#33cc33", "#cccc33", "#3366cc", "#cc33cc", "#33cccc", "#cccccc",
        "#555555", "#ff5555", "#55ff55", "#ffff55", "#5588ff", "#ff55ff", "#55ffff", "#ffffff"]
"##;
    let t = Theme::from_toml(src).unwrap();
    assert_eq!(t.appearance, Appearance::Dark, "guessed from the background");
    assert_eq!(t.terminal.cursor, t.terminal.foreground);
    assert_eq!(t.terminal.cursor_text, t.terminal.background);
    assert_eq!(t.ui, derive_ui(&t.terminal));
}

#[test]
fn partial_ui_overrides_individual_tokens() {
    let src = r##"
name = "Mini"
appearance = "light"
[terminal]
foreground = "#101010"
background = "#f0f0f0"
ansi = ["#000000", "#cc3333", "#33cc33", "#cccc33", "#3366cc", "#cc33cc", "#33cccc", "#cccccc",
        "#555555", "#ff5555", "#55ff55", "#ffff55", "#5588ff", "#ff55ff", "#55ffff", "#ffffff"]
[ui]
accent = "#ff00ff"
"##;
    let t = Theme::from_toml(src).unwrap();
    assert_eq!(t.appearance, Appearance::Light);
    assert_eq!(t.ui.accent, Rgb::new(0xff, 0, 0xff));
    assert_eq!(t.ui.surface, t.terminal.background, "others stay derived");
}

#[test]
fn named_ansi_table_is_accepted() {
    let mut body = String::from(
        "name = \"Named\"\n[terminal]\nforeground = \"#ffffff\"\nbackground = \"#000000\"\n[terminal.ansi]\n",
    );
    for (i, n) in ANSI_NAMES.iter().enumerate() {
        body.push_str(&format!("{n} = \"#{:02x}{:02x}{:02x}\"\n", i * 10, i * 5, 255 - i));
    }
    let t = Theme::from_toml(&body).unwrap();
    assert_eq!(t.terminal.ansi[3], Rgb::new(30, 15, 252));
    assert_eq!(t.terminal.ansi[15], Rgb::new(150, 75, 240));
}

#[test]
fn theme_errors_name_key_and_line() {
    let src =
        "name = \"X\"\n[terminal]\nforeground = \"#fff\"\nbackground = \"#000000\"\nansi = []\n";
    let e = Theme::from_toml(src).unwrap_err();
    assert_eq!(e.key.as_deref(), Some("terminal.foreground"));
    assert_eq!(e.line, Some(3));
    assert!(e.to_string().contains("terminal.foreground"), "{e}");

    let few = "name = \"X\"\n[terminal]\nforeground = \"#ffffff\"\nbackground = \"#000000\"\nansi = [\"#000000\"]\n";
    let e = Theme::from_toml(few).unwrap_err();
    assert_eq!(e.key.as_deref(), Some("terminal.ansi"));
    assert!(e.message.contains("16"), "{e}");

    let mut bad_entry = String::from(
        "name = \"X\"\n[terminal]\nforeground = \"#ffffff\"\nbackground = \"#000000\"\nansi = [\n",
    );
    for i in 0..16 {
        bad_entry.push_str(if i == 5 { "  \"nothex\",\n" } else { "  \"#123456\",\n" });
    }
    bad_entry.push_str("]\n");
    let e = Theme::from_toml(&bad_entry).unwrap_err();
    assert_eq!(e.key.as_deref(), Some("terminal.ansi[5]"));
    assert_eq!(e.line, Some(11));

    let unknown = "name = \"X\"\nbogus = 1\n[terminal]\nforeground = \"#ffffff\"\nbackground = \"#000000\"\nansi = []\n";
    let e = Theme::from_toml(unknown).unwrap_err();
    assert_eq!(e.line, Some(2));
    assert!(e.message.contains("bogus"), "{e}");

    let e = Theme::from_toml("name = \"X\"\n[ui]\nsurface = \"red\"\n[terminal]\nforeground = \"#ffffff\"\nbackground = \"#000000\"\nansi = []\n").unwrap_err();
    assert!(e.key.is_some());
}

#[test]
fn palette_is_plain_data_matching_the_theme() {
    let t = base_dark();
    let p = t.palette();
    assert_eq!(p.foreground, t.terminal.foreground);
    assert_eq!(p.background, t.terminal.background);
    assert_eq!(p.cursor, t.terminal.cursor);
    assert_eq!(p.ansi, t.terminal.ansi);
    assert_eq!(
        (p.selection_bg, p.selection_fg),
        (t.terminal.selection_bg, t.terminal.selection_fg)
    );
}

const GHOSTTY: &str = "\
# A comment
palette = 0=#1d1f21
palette = 1=#cc6666
palette = 2=#b5bd68
palette = 3=#f0c674
palette = 4=#81a2be
palette = 5=#b294bb
palette = 6=#8abeb7
palette = 7=#c5c8c6
palette = 8=#969896
palette = 9=#cc6666
palette = 10=#b5bd68
palette = 11=#f0c674
palette = 12=#81a2be
palette = 13=#b294bb
palette = 14=#8abeb7
palette = 15=#ffffff
background = #1d1f21
foreground = #c5c8c6
cursor-color = #c5c8c6
selection-background = #373b41
selection-foreground = #c5c8c6
font-size = 14
";

#[test]
fn ghostty_import_reads_every_documented_key() {
    let t = Theme::from_ghostty("Imported", GHOSTTY).unwrap();
    assert_eq!(t.name, "Imported");
    assert_eq!(t.terminal.background, Rgb::new(0x1d, 0x1f, 0x21));
    assert_eq!(t.terminal.foreground, Rgb::new(0xc5, 0xc8, 0xc6));
    assert_eq!(t.terminal.cursor, Rgb::new(0xc5, 0xc8, 0xc6));
    assert_eq!(t.terminal.selection_bg, Rgb::new(0x37, 0x3b, 0x41));
    assert_eq!(t.terminal.selection_fg, Rgb::new(0xc5, 0xc8, 0xc6));
    assert_eq!(t.terminal.ansi[1], Rgb::new(0xcc, 0x66, 0x66));
    assert_eq!(t.terminal.ansi[15], Rgb::new(0xff, 0xff, 0xff));
    assert_eq!(t.appearance, Appearance::Dark);
}

#[test]
fn ghostty_import_derives_ui_with_the_documented_algorithm() {
    let t = Theme::from_ghostty("Imported", GHOSTTY).unwrap();
    let (bg, fg) = (t.terminal.background, t.terminal.foreground);
    assert_eq!(t.ui.surface, bg);
    assert_eq!(t.ui.surface_raised, mix(bg, fg, 0.06));
    assert_eq!(t.ui.border, mix(bg, fg, 0.18));
    assert_eq!(t.ui.text, fg);
    assert_eq!(t.ui.text_muted, mix(bg, fg, 0.60));
    assert_eq!(t.ui.tab_active_bg, mix(bg, fg, 0.12));
    assert_eq!(t.ui.accent, t.terminal.ansi[4]);
    assert_eq!(t.ui.border_focused, t.terminal.ansi[4]);
    assert_eq!(t.ui.danger, t.terminal.ansi[1]);
    assert_eq!(t.ui.success, t.terminal.ansi[2]);
    assert_eq!(t.ui.warning, t.terminal.ansi[3]);
    assert_eq!(t.ui.attention, t.terminal.ansi[5]);
    // Pin one number so the algorithm cannot drift unnoticed:
    // 0x1d + (0xc5 - 0x1d) * 0.06 = 29 + 168 * 0.06 = 39.08 -> 0x27.
    assert_eq!(t.ui.surface_raised.r, 0x27);
    // Sunken moves away from fg: 29 - 168 * 0.05 = 20.6 -> 21.
    assert_eq!(t.ui.surface_sunken.r, 21);
}

#[test]
fn ghostty_import_light_background_is_light_and_missing_slots_use_xterm() {
    let t = Theme::from_ghostty(
        "Day",
        "background = #fafafa\nforeground = #202020\npalette = 1=#aa0000\n",
    )
    .unwrap();
    assert_eq!(t.appearance, Appearance::Light);
    assert_eq!(t.terminal.ansi[1], Rgb::new(0xaa, 0, 0));
    assert_eq!(
        t.terminal.ansi[2],
        Rgb::new(0x00, 0xcd, 0x00),
        "missing slot gets the xterm default"
    );
    assert_eq!(t.terminal.cursor, t.terminal.foreground);
}

#[test]
fn ghostty_import_errors_carry_line_numbers() {
    let e = Theme::from_ghostty(
        "x",
        "background = #000000\nforeground = #ffffff\npalette = 3=#12345\n",
    )
    .unwrap_err();
    assert_eq!((e.key.as_deref(), e.line), (Some("palette"), Some(3)));
    let e = Theme::from_ghostty("x", "background = #000000\nforeground = oops\n").unwrap_err();
    assert_eq!((e.key.as_deref(), e.line), (Some("foreground"), Some(2)));
    let e = Theme::from_ghostty("x", "background = #000000\n").unwrap_err();
    assert_eq!(e.key.as_deref(), Some("foreground"));
    let e = Theme::from_ghostty("x", "background = #000000\nforeground = #fff000\npalette = 7\n")
        .unwrap_err();
    assert!(e.message.contains("N=#rrggbb"), "{e}");
}

fn theme_toml(name: &str, bg: &str) -> String {
    format!(
        "name = \"{name}\"\n[terminal]\nforeground = \"#ffffff\"\nbackground = \"{bg}\"\nansi = [\"#000000\", \"#cc3333\", \"#33cc33\", \"#cccc33\", \"#3366cc\", \"#cc33cc\", \"#33cccc\", \"#cccccc\", \"#555555\", \"#ff5555\", \"#55ff55\", \"#ffff55\", \"#5588ff\", \"#ff55ff\", \"#55ffff\", \"#ffffff\"]\n"
    )
}

#[test]
fn user_themes_load_and_override_bundled_by_name() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.toml"), theme_toml("My Theme", "#101820")).unwrap();
    fs::write(dir.path().join("b.toml"), theme_toml("kelp", "#000a00")).unwrap();
    fs::write(dir.path().join("ignored.txt"), "not a theme").unwrap();
    let mut reg = ThemeRegistry::bundled();
    let errors = reg.load_user_dir(dir.path());
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(reg.names().len(), 7, "one new, one override");
    assert_eq!(reg.entry("My Theme").unwrap().origin, Origin::User);
    let kelp = reg.entry("Kelp").unwrap();
    assert_eq!(kelp.origin, Origin::User, "user theme overrides the bundled one of the same name");
    assert_eq!(kelp.theme.terminal.background, Rgb::new(0, 0x0a, 0));
    assert_eq!(reg.entry("Harbor Fog").unwrap().origin, Origin::Bundled);
    assert_eq!(
        reg.names().iter().position(|n| *n == "kelp" || *n == "Kelp").unwrap(),
        3,
        "override keeps its slot"
    );
}

#[test]
fn bad_user_theme_is_reported_and_does_not_block_the_others() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a-bad.toml"), "name = \"Bad\"\n[terminal]\nforeground = \"x\"\n")
        .unwrap();
    fs::write(dir.path().join("b-good.toml"), theme_toml("Good", "#101820")).unwrap();
    let mut reg = ThemeRegistry::bundled();
    let errors = reg.load_user_dir(dir.path());
    assert_eq!(errors.len(), 1);
    let shown = errors[0].to_string();
    assert!(shown.contains("a-bad.toml"), "{shown}");
    assert!(reg.get("Good").is_some());
    assert!(reg.get("Bad").is_none());
}

#[test]
fn missing_user_theme_dir_is_fine() {
    let dir = tempfile::tempdir().unwrap();
    let mut reg = ThemeRegistry::bundled();
    assert!(reg.load_user_dir(&dir.path().join("themes")).is_empty());
    assert_eq!(reg.names().len(), 6);
}

#[test]
fn lookup_is_case_insensitive_and_falls_back_to_the_default() {
    let reg = ThemeRegistry::bundled();
    assert!(reg.get("halyard NIGHT").is_some());
    let (t, fell_back) = reg.get_or_default("No Such Theme", Appearance::Light);
    assert!(fell_back);
    assert_eq!(t.name, DEFAULT_LIGHT);
    let (t, fell_back) = reg.get_or_default("kelp", Appearance::Light);
    assert!(!fell_back);
    assert_eq!(t.name, "Kelp");
}
