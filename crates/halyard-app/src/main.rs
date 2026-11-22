//! `halyard`: the terminal application.
//!
//! One window per session. Settings come from `config.toml` (see
//! `docs/configuration.md`) and apply live when the file changes; key chords
//! come from the config's keymap. Problems with the config show in a banner
//! at the bottom of the window and never stop the terminal.
//!
//! When the shell exits with status 0 the window closes. Any other exit
//! leaves the window open with a notice, and the next key press closes it.
//! The app quits when its last window closes.

mod app_state;
#[cfg(feature = "debug-script")]
mod debug_script;
mod decide;
mod grid;
mod keybind;
mod keys;
mod procinfo;
mod selection;
mod settings;
mod view;

use gpui::{
    AnyElement, App, Application, Bounds, Context, Entity, FocusHandle, Focusable, Menu, MenuItem,
    Subscription, Task, TitlebarOptions, Window, WindowAppearance, WindowBounds, WindowOptions,
    div, prelude::*, px, size,
};
use halyard_config::{Action, Theme};

use crate::app_state::AppState;
use crate::keybind::*;
use crate::view::{PaneEvent, PaneSpawn, PaneStyle, TerminalView, to_hsla};

/// The app id (Wayland) and WM_CLASS instance and class (X11), matching
/// `StartupWMClass` in the desktop entry so taskbars group the windows.
pub const APP_ID: &str = "halyard";

/// Height of the notice banner.
const BANNER_H: f32 = 26.0;

/// Cursor blink half-period.
const BLINK: std::time::Duration = std::time::Duration::from_millis(530);

/// The initial window size from `initial_cols` and `initial_rows`.
fn initial_size(cx: &App) -> gpui::Size<gpui::Pixels> {
    let s = &cx.global::<AppState>().settings;
    let font = s.font();
    let w = &s.config.window;
    // Close enough before a window exists to measure the font: monospace
    // advances are about 0.6 em. The grid refits on the first paint.
    let cell_w = font.size * 0.6;
    let cell_h = (font.size * font.line_height).ceil();
    let width = f32::from(w.initial_cols) * cell_w + 2.0 * w.padding_x;
    let height = f32::from(w.initial_rows) * cell_h + 2.0 * w.padding_y;
    size(px(width.clamp(320.0, 3000.0)), px(height.clamp(200.0, 2000.0)))
}

fn is_dark(a: WindowAppearance) -> bool {
    matches!(a, WindowAppearance::Dark | WindowAppearance::VibrantDark)
}

/// Window root: one terminal pane, the config banner, and the actions.
struct Root {
    pane: Option<Entity<TerminalView>>,
    error: Option<String>,
    focus: FocusHandle,
    local_notices: Vec<String>,
    banner_hidden: bool,
    applied_generation: Option<u64>,
    dark: bool,
    font_delta: f32,
    blink_on: bool,
    _blink: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl Root {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subs = vec![
            cx.observe_global_in::<AppState>(window, |this, window, cx| {
                let generation = cx.global::<AppState>().settings.generation;
                if this.applied_generation != Some(generation) {
                    this.applied_generation = Some(generation);
                    this.banner_hidden = false;
                    this.local_notices.retain(|n| !n.starts_with("config: key `font.family`"));
                    this.apply_settings(window, cx);
                }
            }),
            cx.observe_window_appearance(window, |this, window, cx| {
                this.dark = is_dark(window.appearance());
                this.apply_settings(window, cx);
            }),
            cx.observe_window_activation(window, |this, _, cx| {
                if let Some(p) = &this.pane {
                    p.update(cx, |_, cx| cx.notify());
                }
            }),
        ];
        let blink = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(BLINK).await;
                if this.update_in(cx, |this, window, cx| this.blink(window, cx)).is_err() {
                    break;
                }
            }
        });
        let mut root = Self {
            pane: None,
            error: None,
            focus: cx.focus_handle(),
            local_notices: Vec::new(),
            banner_hidden: false,
            applied_generation: Some(cx.global::<AppState>().settings.generation),
            dark: is_dark(window.appearance()),
            font_delta: 0.0,
            blink_on: true,
            _blink: blink,
            _subscriptions: subs,
        };
        root.start_pane(window, cx);
        #[cfg(feature = "debug-script")]
        if !cx.global::<AppState>().script_started {
            cx.global_mut::<AppState>().script_started = true;
            crate::debug_script::start(window, cx);
        }
        root
    }

    fn theme(&self, cx: &App) -> Theme {
        cx.global::<AppState>().settings.theme(settings::appearance(self.dark)).0.clone()
    }

    fn style(&mut self, window: &Window, cx: &App) -> PaneStyle {
        let s = &cx.global::<AppState>().settings;
        let font = s.font();
        let installed = window.text_system().all_font_names();
        let (family, notice) = view::resolve_font_family(&font.family, &installed);
        if let Some(n) = notice
            && !self.local_notices.contains(&n)
        {
            self.local_notices.push(n);
            self.banner_hidden = false;
        }
        let t = &s.config.terminal;
        let w = &s.config.window;
        PaneStyle {
            family,
            font_size: settings::step_font_size(font.size, self.font_delta),
            line_height: font.line_height,
            padding_x: w.padding_x,
            padding_y: w.padding_y,
            cursor: settings::cursor_shape(t.cursor_style),
            blink: t.cursor_blink,
            cursor_text: self.theme(cx).terminal.cursor_text,
            copy_on_select: t.copy_on_select,
            confirm_multiline_paste: t.confirm_multiline_paste,
        }
    }

    fn start_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let style = self.style(window, cx);
        let theme = self.theme(cx);
        let s = &cx.global::<AppState>().settings;
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        let spawn = PaneSpawn {
            command: settings::shell_command(&s.config),
            cwd: settings::start_directory(
                &s.config.terminal.working_directory,
                None,
                None,
                home.as_deref(),
            ),
            scrollback: usize::try_from(s.config.terminal.scrollback_lines).unwrap_or(usize::MAX),
            palette: settings::engine_palette(&theme.palette()),
        };
        match TerminalView::prepare(window, style, spawn, window.viewport_size()) {
            Ok(prepared) => {
                let pane = cx.new(|cx| TerminalView::new(prepared, window, cx));
                cx.subscribe_in(&pane, window, |this, _, ev, window, cx| match ev {
                    PaneEvent::Exited | PaneEvent::CloseRequested => window.remove_window(),
                    PaneEvent::TitleChanged => this.update_title(window, cx),
                    PaneEvent::Focused => {}
                })
                .detach();
                window.focus(&pane.focus_handle(cx));
                self.pane = Some(pane);
                self.update_title(window, cx);
            }
            Err(e) => {
                eprintln!("halyard: {e}");
                self.error = Some(e.to_string());
            }
        }
    }

    fn update_title(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = &self.pane else { return };
        let v = p.read(cx);
        let title = if v.title().trim().is_empty() {
            v.cwd()
                .and_then(|c| {
                    std::path::Path::new(&c).file_name().map(|f| f.to_string_lossy().into_owned())
                })
                .unwrap_or_else(|| "Halyard".into())
        } else {
            v.title().trim().to_owned()
        };
        window.set_window_title(&title);
    }

    fn apply_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let style = self.style(window, cx);
        let palette = settings::engine_palette(&self.theme(cx).palette());
        if let Some(p) = &self.pane {
            p.update(cx, |v, cx| {
                v.set_style(style, window, cx);
                v.set_palette(palette);
            });
        }
        cx.notify();
    }

    fn blink(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = &self.pane else { return };
        let wants = p.read(cx).wants_blink(window);
        self.blink_on = if wants { !self.blink_on } else { true };
        let on = self.blink_on;
        p.update(cx, |v, cx| {
            v.set_blink(on, cx);
        });
    }

    fn run(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            Action::ClosePane => self.close(window, cx),
            Action::IncreaseFontSize | Action::DecreaseFontSize | Action::ResetFontSize => {
                let base = cx.global::<AppState>().settings.font().size;
                self.font_delta = match action {
                    Action::IncreaseFontSize => {
                        settings::step_font_size(base, self.font_delta + 1.0) - base
                    }
                    Action::DecreaseFontSize => {
                        settings::step_font_size(base, self.font_delta - 1.0) - base
                    }
                    _ => 0.0,
                };
                self.apply_settings(window, cx);
            }
            Action::ReloadConfig => app_state::reload_config(cx),
            Action::OpenConfig => {
                if let Err(e) = app_state::open_config(cx) {
                    self.local_notices.push(e);
                    self.banner_hidden = false;
                    cx.notify();
                }
            }
            Action::NewWindow => open_window(cx),
            Action::Quit => cx.quit(),
            _ => {}
        }
    }

    /// Close the window, asking first if a command is running in it.
    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let setting = cx.global::<AppState>().settings.config.terminal.confirm_close_running;
        let busy = self.pane.as_ref().is_some_and(|p| p.read(cx).needs_close_confirmation(setting));
        if !busy {
            window.remove_window();
            return;
        }
        let answer = window.prompt(
            gpui::PromptLevel::Warning,
            "Close this window?",
            Some("A command is still running in it, and closing will end it."),
            &["Close", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |_, cx| {
            if answer.await == Ok(0) {
                cx.update(|window, _| window.remove_window()).ok();
            }
        })
        .detach();
    }

    fn notices(&self, cx: &App) -> Vec<String> {
        let mut v = cx.global::<AppState>().settings.notices.clone();
        v.extend(self.local_notices.iter().cloned());
        v
    }

    fn render_banner(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.banner_hidden {
            return None;
        }
        let notices = self.notices(cx);
        let first = notices.first()?.clone();
        let more = notices.len() - 1;
        let ui = &theme.ui;
        let text = if more > 0 { format!("{first}  (+{more} more)") } else { first };
        Some(
            div()
                .id("banner")
                .h(px(BANNER_H))
                .w_full()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_3()
                .bg(to_hsla(ui.surface_raised))
                .border_t_1()
                .border_color(to_hsla(ui.warning))
                .text_xs()
                .text_color(to_hsla(ui.text))
                .child(div().text_color(to_hsla(ui.warning)).child("⚠"))
                .child(div().flex_1().overflow_hidden().whitespace_nowrap().child(text))
                .child(
                    div()
                        .id("banner-close")
                        .px_1()
                        .text_color(to_hsla(ui.text_muted))
                        .child("×")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.banner_hidden = true;
                            cx.notify();
                        })),
                )
                .into_any_element(),
        )
    }
}

macro_rules! on_actions {
    ($el:expr, $cx:expr, $($ty:ident),+ $(,)?) => {{
        let el = $el;
        $(
            let el = el.on_action($cx.listener(|this, _: &$ty, window, cx| {
                this.run(Action::$ty, window, cx)
            }));
        )+
        el
    }};
}

impl Render for Root {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme(cx);
        let root = div()
            .key_context("Root")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .bg(to_hsla(theme.ui.surface))
            .text_color(to_hsla(theme.ui.text))
            .font_family(".SystemUIFont");
        let root = on_actions!(
            root,
            cx,
            ClosePane,
            IncreaseFontSize,
            DecreaseFontSize,
            ResetFontSize,
            ReloadConfig,
            OpenConfig,
            NewWindow,
            Quit,
        );
        let banner = self.render_banner(&theme, cx);
        root.when_some(self.pane.clone(), |el, p| el.child(div().flex_1().child(p)))
            .when_some(self.error.clone(), |el, e| {
                el.p_4().child(format!("Halyard could not start a shell: {e}"))
            })
            .children(banner)
    }
}

impl Focusable for Root {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

#[cfg(feature = "debug-script")]
impl debug_script::Host for Root {
    fn script_dump(&mut self, cx: &mut Context<Self>) -> String {
        let Some(p) = &self.pane else { return "no pane".into() };
        let v = p.read(cx);
        let f = v.session().latest_frame();
        format!(
            "frame {} ({}x{}), renders {}:\n{}",
            f.generation,
            f.cols,
            f.rows,
            v.renders,
            f.text()
        )
    }

    fn script_select(&mut self, c1: u16, r1: u16, c2: u16, r2: u16, cx: &mut Context<Self>) {
        if let Some(p) = &self.pane {
            p.update(cx, |v, cx| {
                v.select_cells(c1, r1, c2, r2);
                cx.notify();
            });
        }
    }

    fn script_action(
        &mut self,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let a = Action::from_name(name).ok_or_else(|| format!("unknown action `{name}`"))?;
        let action = keybind::gpui_action(a).ok_or_else(|| format!("`{name}` is not built"))?;
        window.dispatch_action(action, cx);
        Ok(())
    }
}

fn open_window(cx: &mut App) {
    let bounds = Bounds::centered(None, initial_size(cx), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions { title: Some("Halyard".into()), ..Default::default() }),
        window_min_size: Some(size(px(200.0), px(100.0))),
        app_id: Some(APP_ID.to_owned()),
        ..Default::default()
    };
    if let Err(e) = cx.open_window(options, |window, cx| cx.new(|cx| Root::new(window, cx))) {
        eprintln!("halyard: could not open a window: {e:#}");
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        AppState::init(cx);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_action(|_: &NewWindow, cx| open_window(cx));
        cx.set_menus(vec![
            Menu {
                name: "Halyard".into(),
                items: vec![
                    MenuItem::action("New Window", NewWindow),
                    MenuItem::action("Open Config File", OpenConfig),
                    MenuItem::action("Reload Config", ReloadConfig),
                    MenuItem::separator(),
                    MenuItem::action("Quit Halyard", Quit),
                ],
            },
            Menu {
                name: "Edit".into(),
                items: vec![MenuItem::action("Copy", Copy), MenuItem::action("Paste", Paste)],
            },
            Menu {
                name: "View".into(),
                items: vec![
                    MenuItem::action("Bigger", IncreaseFontSize),
                    MenuItem::action("Smaller", DecreaseFontSize),
                    MenuItem::action("Actual Size", ResetFontSize),
                ],
            },
        ]);
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        open_window(cx);
        cx.activate(true);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_appearances() {
        assert!(is_dark(WindowAppearance::Dark));
        assert!(is_dark(WindowAppearance::VibrantDark));
        assert!(!is_dark(WindowAppearance::Light));
        assert!(!is_dark(WindowAppearance::VibrantLight));
    }
}
