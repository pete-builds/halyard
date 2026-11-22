//! `halyard`: the terminal application.
//!
//! One window per session. Shortcuts: on macOS Cmd+C copies the selection,
//! Cmd+V pastes, Cmd+N opens a window, Cmd+W closes it, Cmd+Q quits. On
//! Linux, Ctrl+Shift+C and Ctrl+Shift+V copy and paste, Ctrl+Shift+N opens
//! a window. Everything else goes to the terminal.
//!
//! When the shell exits with status 0 the window closes. Any other exit
//! leaves the window open with a notice, and the next key press closes it.
//! The app quits when its last window closes.

#[cfg(feature = "debug-script")]
mod debug_script;
mod grid;
mod keys;
mod view;

use gpui::{
    App, Application, Bounds, Focusable, KeyBinding, Menu, MenuItem, TitlebarOptions, WindowBounds,
    WindowOptions, actions, prelude::*, px, size,
};

use crate::view::TerminalView;

actions!(halyard, [Copy, Paste, NewWindow, CloseWindow, Quit]);

fn open_terminal_window(cx: &mut App) {
    let bounds = Bounds::centered(None, size(px(900.0), px(560.0)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions { title: Some("Halyard".into()), ..Default::default() }),
        window_min_size: Some(size(px(200.0), px(100.0))),
        ..Default::default()
    };
    let result = cx.open_window(options, |window, cx| {
        let root = match TerminalView::prepare(window) {
            Ok(prepared) => {
                let terminal = cx.new(|cx| TerminalView::new(prepared, window, cx));
                window.focus(&terminal.focus_handle(cx));
                Root { terminal: Some(terminal), error: None }
            }
            Err(e) => {
                eprintln!("halyard: {e}");
                Root { terminal: None, error: Some(e.to_string()) }
            }
        };
        cx.new(|_| root)
    });
    if let Err(e) = result {
        eprintln!("halyard: could not open a window: {e:#}");
    }
}

/// Window root: the terminal, or the reason it failed to start.
struct Root {
    terminal: Option<gpui::Entity<TerminalView>>,
    error: Option<String>,
}

impl Render for Root {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement {
        gpui::div()
            .size_full()
            .on_action(|_: &CloseWindow, window, _| window.remove_window())
            .when_some(self.terminal.clone(), |el, t| el.child(t))
            .when_some(self.error.clone(), |el, e| {
                el.p_4().child(format!("Halyard could not start a shell: {e}"))
            })
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        let mut bindings = vec![
            KeyBinding::new("cmd-c", Copy, None),
            KeyBinding::new("cmd-v", Paste, None),
            KeyBinding::new("cmd-n", NewWindow, None),
            KeyBinding::new("cmd-w", CloseWindow, None),
            KeyBinding::new("cmd-q", Quit, None),
        ];
        if cfg!(not(target_os = "macos")) {
            bindings = vec![
                KeyBinding::new("ctrl-shift-c", Copy, None),
                KeyBinding::new("ctrl-shift-v", Paste, None),
                KeyBinding::new("ctrl-shift-n", NewWindow, None),
                KeyBinding::new("ctrl-shift-w", CloseWindow, None),
                KeyBinding::new("ctrl-shift-q", Quit, None),
            ];
        }
        cx.bind_keys(bindings);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_action(|_: &NewWindow, cx| open_terminal_window(cx));
        cx.set_menus(vec![
            Menu {
                name: "Halyard".into(),
                items: vec![
                    MenuItem::action("New Window", NewWindow),
                    MenuItem::action("Quit Halyard", Quit),
                ],
            },
            Menu {
                name: "Edit".into(),
                items: vec![MenuItem::action("Copy", Copy), MenuItem::action("Paste", Paste)],
            },
        ]);
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        open_terminal_window(cx);
        cx.activate(true);
    });
}
