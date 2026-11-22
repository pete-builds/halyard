//! `halyard`: the terminal application.
//!
//! Windows hold tabs, tabs hold split panes, each pane runs its own shell.
//! Settings come from `config.toml` (see `docs/configuration.md`) and apply
//! live when the file changes. Key chords come from the config's keymap.
//! The layout of every window is saved (layout and working directories
//! only) and restored on the next launch.
//!
//! When a shell exits with status 0 its pane closes. Any other exit leaves
//! the pane open with a notice, and the next key press closes it. The last
//! pane closes its tab, the last tab its window, and the app quits when its
//! last window closes.

mod app_state;
#[cfg(feature = "debug-script")]
mod debug_script;
mod decide;
mod grid;
mod keybind;
mod keys;
mod palette;
mod procinfo;
mod selection;
mod settings;
mod view;
mod workspace_view;

use gpui::{
    App, Application, Bounds, Focusable, Menu, MenuItem, TitlebarOptions, WindowBounds,
    WindowOptions, prelude::*, px, size,
};
use halyard_workspace::{Restored, Workspace, persist};

use crate::app_state::AppState;
use crate::keybind::*;
use crate::workspace_view::{TAB_BAR_H, WorkspaceView};

/// The app id (Wayland) and WM_CLASS instance and class (X11), matching
/// `StartupWMClass` in the desktop entry so taskbars group the windows.
pub const APP_ID: &str = "halyard";

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
    let height = f32::from(w.initial_rows) * cell_h + 2.0 * w.padding_y + TAB_BAR_H;
    size(px(width.clamp(320.0, 3000.0)), px(height.clamp(200.0, 2000.0)))
}

/// Open a window with a restored layout, or a fresh one with `None`.
pub fn open_window(restored: Option<Restored>, cx: &mut App) {
    let restored = restored.unwrap_or_else(|| {
        let (workspace, pane) = Workspace::new();
        Restored { workspace, cwds: vec![(pane, None)] }
    });
    let bounds = Bounds::centered(None, initial_size(cx), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions { title: Some("Halyard".into()), ..Default::default() }),
        window_min_size: Some(size(px(240.0), px(140.0))),
        app_id: Some(APP_ID.to_owned()),
        ..Default::default()
    };
    let result = cx.open_window(options, |window, cx| {
        let view = cx.new(|cx| WorkspaceView::new(restored, window, cx));
        window.focus(&view.focus_handle(cx));
        cx.global_mut::<AppState>().windows.push(view.downgrade());
        view
    });
    match result {
        Ok(handle) => {
            // Focus the active pane now that the window exists.
            handle
                .update(cx, |view, window, cx| {
                    view.run_focus(window, cx);
                })
                .ok();
        }
        Err(e) => eprintln!("halyard: could not open a window: {e:#}"),
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        AppState::init(cx);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_action(|_: &NewWindow, cx| open_window(None, cx));
        cx.set_menus(vec![
            Menu {
                name: "Halyard".into(),
                items: vec![
                    MenuItem::action("Command Palette", CommandPalette),
                    MenuItem::action("Open Config File", OpenConfig),
                    MenuItem::action("Reload Config", ReloadConfig),
                    MenuItem::separator(),
                    MenuItem::action("Quit Halyard", Quit),
                ],
            },
            Menu {
                name: "Shell".into(),
                items: vec![
                    MenuItem::action("New Window", NewWindow),
                    MenuItem::action("New Tab", NewTab),
                    MenuItem::action("Split Right", SplitRight),
                    MenuItem::action("Split Down", SplitDown),
                    MenuItem::separator(),
                    MenuItem::action("Close Pane", ClosePane),
                ],
            },
            Menu {
                name: "Edit".into(),
                items: vec![MenuItem::action("Copy", Copy), MenuItem::action("Paste", Paste)],
            },
            Menu {
                name: "View".into(),
                items: vec![
                    MenuItem::action("Zoom Pane", ZoomPane),
                    MenuItem::action("Bigger", IncreaseFontSize),
                    MenuItem::action("Smaller", DecreaseFontSize),
                    MenuItem::action("Actual Size", ResetFontSize),
                    MenuItem::action("Switch Theme", SwitchTheme),
                    MenuItem::action("Rename Tab", RenameTab),
                ],
            },
        ]);
        cx.on_app_quit(|cx| {
            app_state::save_now(cx);
            async {}
        })
        .detach();
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let loaded = match cx.global::<AppState>().state_file.clone() {
            Some(p) => persist::load(&p),
            None => persist::LoadResult { windows: Vec::new(), warnings: Vec::new() },
        };
        cx.global_mut::<AppState>().restore_warnings =
            loaded.warnings.iter().map(|w| format!("workspace: {w}")).collect();
        for restored in loaded.windows_or_default() {
            open_window(Some(restored), cx);
        }
        cx.activate(true);
    });
}
