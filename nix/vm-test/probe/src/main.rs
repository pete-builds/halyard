//! Opens one window showing a fixed string, large and high contrast, so the
//! VM test can find it with OCR.

use gpui::{
    App, Application, Bounds, Context, SharedString, Window, WindowBounds, WindowOptions, div,
    prelude::*, px, rgb, size,
};

const MARKER: &str = "HALYARD-VM-OK";

struct Probe {
    text: SharedString,
}

impl Render for Probe {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .size_full()
            .justify_center()
            .items_center()
            .bg(rgb(0xffffff))
            .text_color(rgb(0x000000))
            .text_size(px(64.0))
            .child(self.text.clone())
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1000.0), px(400.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| {
                cx.new(|_| Probe {
                    text: MARKER.into(),
                })
            },
        )
        .expect("open the probe window");
        eprintln!("halyard-vm-probe: window opened");
        cx.activate(true);
    });
}
