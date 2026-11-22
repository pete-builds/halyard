//! Debug-only scripted driving, for verifying the window without a person
//! at the keyboard. Compiled only with the `debug-script` feature.
//!
//! `HALYARD_DEBUG_SCRIPT=<file>` runs one command per line after launch:
//!
//! - `sleep <ms>`: wait.
//! - `type <text>`: type each character as a key press through the window's
//!   real dispatch (key bindings, then the focused element, then text input).
//! - `key <keystroke>`: press a key the same way, written in GPUI keystroke
//!   syntax (`enter`, `ctrl-c`, `escape`, `space`, `cmd-shift-p`).
//! - `action <name>`: run a `[keybindings]` action by its config name.
//! - `mousedown <x> <y>`, `mousemove <x> <y>`, `mouseup <x> <y>`: left
//!   button events at window coordinates (logical px from the top left),
//!   posted to the window through AppKit (macOS only), so they take the same
//!   path as a real mouse and drags work.
//! - `rightdown <x> <y>`, `rightup <x> <y>`: the same for the right button.
//!   AppKit's event factory cannot set a button number, which GPUI reads,
//!   so these post a Control-click: GPUI turns that into the same right
//!   button event a mouse's right button gives (Control cleared).
//! - Any mouse command takes optional words after the position: `shift`
//!   holds Shift, and `clicks=<n>` sets the click count (`clicks=2` is the
//!   second press of a double click).
//! - `copy <from> <to>`: copy a file (for swapping a config file mid-run).
//! - `shot <path.png>`: save a PNG of this window (macOS only; captures the
//!   app's own window, so no screen recording permission is needed).
//! - `select <col> <row> <col> <row>`: select from one cell to another in the
//!   focused pane (through the session, not the mouse path).
//! - `resize <width> <height>`: resize the window's content (logical px).
//! - `activate`: bring the app and this window to the front.
//! - `closebutton`: press the window's close button (`-[NSWindow performClose:]`).
//! - `dump`: print the focused pane's frame text and render count to stderr.
//! - `quit`: quit the app.
//!
//! Lines starting with `#` and blank lines are ignored.

use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::{Context, Keystroke, Modifiers, Render, Window, px, size};

/// One step of a debug script.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// Wait.
    Sleep(Duration),
    /// Type text.
    Type(String),
    /// Press a key (GPUI keystroke syntax).
    Key(String),
    /// Save a screenshot of the window.
    Shot(PathBuf),
    /// Select between two cells.
    Select(u16, u16, u16, u16),
    /// Resize the window content to a width and height in logical pixels.
    Resize(f32, f32),
    /// Print the latest frame's text.
    Dump,
    /// Run a config action by name.
    Action(String),
    /// A mouse event at a window position.
    Mouse(Mouse),
    /// Copy a file.
    Copy(PathBuf, PathBuf),
    /// Bring the window to the front.
    Activate,
    /// Press the close button.
    CloseButton,
    /// Quit.
    Quit,
}

/// A mouse button a script can press.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    /// The left (primary) button.
    Left,
    /// The right (secondary) button.
    Right,
}

/// One scripted mouse event.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mouse {
    /// Which button.
    pub button: Button,
    /// Pressed, dragged or released.
    pub kind: MouseKind,
    /// Window position, logical px from the top left.
    pub x: f32,
    /// Window position, logical px from the top left.
    pub y: f32,
    /// Click count (2 for the second press of a double click).
    pub clicks: u8,
    /// Shift held.
    pub shift: bool,
}

/// Parse `<x> <y> [shift] [clicks=<n>]`.
fn mouse_args(
    arg: &str,
    n: usize,
    what: &str,
    button: Button,
    kind: MouseKind,
) -> Result<Mouse, String> {
    let words: Vec<&str> = arg.split_whitespace().collect();
    let (pos, rest) = words.split_at(words.len().min(2));
    let (x, y) = two_numbers(&pos.join(" "), n, what)?;
    let mut m = Mouse { button, kind, x, y, clicks: 1, shift: false };
    for w in rest {
        if *w == "shift" {
            m.shift = true;
        } else if let Some(c) = w.strip_prefix("clicks=") {
            m.clicks = c.parse().map_err(|e| format!("line {}: clicks: {e}", n + 1))?;
        } else {
            return Err(format!("line {}: {what}: unknown option `{w}`", n + 1));
        }
    }
    Ok(m)
}

fn two_numbers(arg: &str, n: usize, what: &str) -> Result<(f32, f32), String> {
    let v: Vec<f32> = arg
        .split_whitespace()
        .map(str::parse)
        .collect::<Result<_, _>>()
        .map_err(|e| format!("line {}: {e}", n + 1))?;
    let [a, b] = v[..] else {
        return Err(format!("line {}: {what} needs two numbers", n + 1));
    };
    Ok((a, b))
}

/// Parse a script, rejecting unknown commands with their line number.
pub fn parse(script: &str) -> Result<Vec<Step>, String> {
    let mut steps = Vec::new();
    for (n, line) in script.lines().enumerate() {
        let line = line.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (cmd, arg) = line.split_once(' ').unwrap_or((line, ""));
        steps.push(match cmd {
            "sleep" => Step::Sleep(Duration::from_millis(
                arg.trim().parse().map_err(|e| format!("line {}: {e}", n + 1))?,
            )),
            "type" => Step::Type(arg.to_owned()),
            "key" => Step::Key(arg.trim().to_owned()),
            "shot" => Step::Shot(PathBuf::from(arg.trim())),
            "select" => {
                let cells: Vec<u16> = arg
                    .split_whitespace()
                    .map(str::parse)
                    .collect::<Result<_, _>>()
                    .map_err(|e| format!("line {}: {e}", n + 1))?;
                let [c1, r1, c2, r2] = cells[..] else {
                    return Err(format!("line {}: select needs four numbers", n + 1));
                };
                Step::Select(c1, r1, c2, r2)
            }
            "resize" => {
                let (w, h) = two_numbers(arg, n, "resize")?;
                Step::Resize(w, h)
            }
            "mousedown" => Step::Mouse(mouse_args(arg, n, cmd, Button::Left, MouseKind::Down)?),
            "mousemove" => Step::Mouse(mouse_args(arg, n, cmd, Button::Left, MouseKind::Dragged)?),
            "mouseup" => Step::Mouse(mouse_args(arg, n, cmd, Button::Left, MouseKind::Up)?),
            "rightdown" => Step::Mouse(mouse_args(arg, n, cmd, Button::Right, MouseKind::Down)?),
            "rightup" => Step::Mouse(mouse_args(arg, n, cmd, Button::Right, MouseKind::Up)?),
            "action" => Step::Action(arg.trim().to_owned()),
            "copy" => {
                let parts: Vec<&str> = arg.split_whitespace().collect();
                let [from, to] = parts[..] else {
                    return Err(format!("line {}: copy needs two paths", n + 1));
                };
                Step::Copy(PathBuf::from(from), PathBuf::from(to))
            }
            "dump" => Step::Dump,
            "activate" => Step::Activate,
            "closebutton" => Step::CloseButton,
            "quit" => Step::Quit,
            other => return Err(format!("line {}: unknown command `{other}`", n + 1)),
        });
    }
    Ok(steps)
}

/// What a scripted window must provide beyond GPUI's own dispatch.
pub trait Host: Render + Sized {
    /// The focused pane's frame text and render count.
    fn script_dump(&mut self, window: &mut Window, cx: &mut Context<Self>) -> String;
    /// Select between two cells in the focused pane.
    fn script_select(&mut self, c1: u16, r1: u16, c2: u16, r2: u16, cx: &mut Context<Self>);
    /// Run a config action by name.
    ///
    /// # Errors
    /// An unknown or unbuilt action name.
    fn script_action(
        &mut self,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String>;
}

fn keystroke_for_char(c: char) -> Keystroke {
    let key = match c {
        ' ' => "space".to_owned(),
        '\n' => "enter".to_owned(),
        c => c.to_lowercase().collect(),
    };
    let modifiers = Modifiers { shift: c.is_uppercase(), ..Modifiers::default() };
    let key_char = (c != '\n').then(|| c.to_string());
    Keystroke { modifiers, key, key_char }
}

/// Run `HALYARD_DEBUG_SCRIPT` against this window, if the variable is set.
pub fn start<H: Host + 'static>(window: &mut Window, cx: &mut Context<H>) {
    let Some(file) = std::env::var_os("HALYARD_DEBUG_SCRIPT") else { return };
    let steps =
        match std::fs::read_to_string(&file).map_err(|e| e.to_string()).and_then(|s| parse(&s)) {
            Ok(steps) => steps,
            Err(e) => {
                // A scripted run with a broken script would otherwise sit idle forever.
                eprintln!("halyard: debug script: {e}");
                cx.quit();
                return;
            }
        };
    cx.spawn_in(window, async move |this, cx| {
        for step in steps {
            let result = match step {
                Step::Sleep(d) => {
                    cx.background_executor().timer(d).await;
                    Ok(())
                }
                Step::Type(text) => cx
                    .update(|window, cx| {
                        for c in text.chars() {
                            window.dispatch_keystroke(keystroke_for_char(c), cx);
                        }
                    })
                    .map_err(|e| e.to_string()),
                Step::Key(k) => match Keystroke::parse(&k) {
                    Ok(ks) => cx
                        .update(|window, cx| {
                            window.dispatch_keystroke(ks, cx);
                        })
                        .map_err(|e| e.to_string()),
                    Err(e) => Err(e.to_string()),
                },
                Step::Mouse(m) => {
                    let target = cx
                        .update(|window, _| {
                            native_view(window)
                                .map(|v| (v, f32::from(window.viewport_size().height)))
                        })
                        .map_err(|e| e.to_string())
                        .and_then(|v| v);
                    // Outside any GPUI update: the window handles the event
                    // synchronously and updates itself. Draw first, because
                    // GPUI hit-tests against the last drawn frame and stops
                    // drawing while the window is occluded.
                    target.and_then(|(view, height)| {
                        force_display(view);
                        post_mouse(view, Mouse { y: height - m.y, ..m })
                    })
                }
                Step::Action(name) => this
                    .update_in(cx, |host, window, cx| host.script_action(&name, window, cx))
                    .map_err(|e| e.to_string())
                    .and_then(|r| r),
                Step::Copy(from, to) => std::fs::copy(&from, &to)
                    .map(|_| eprintln!("halyard: copied {} to {}", from.display(), to.display()))
                    .map_err(|e| e.to_string()),
                Step::Shot(p) => {
                    // Mark the window dirty so the forced draw below renders
                    // and presents the current state: a frame drawn while the
                    // window was hidden may never have reached the screen.
                    let view = cx
                        .update(|window, _| {
                            window.refresh();
                            native_view(window)
                        })
                        .map_err(|e| e.to_string())
                        .and_then(|v| v);
                    match view {
                        Ok(view) => {
                            // Outside the update: the draw it triggers updates
                            // the window. Then let Core Animation commit.
                            // Draw twice: a draw can send a resize (after a
                            // font change), and the reflowed frame arrives
                            // a moment later, as it would in a visible window.
                            force_display(view);
                            cx.background_executor().timer(Duration::from_millis(150)).await;
                            if let Ok(Ok(view)) = cx.update(|window, _| native_view(window)) {
                                force_display(view);
                            }
                            cx.background_executor().timer(Duration::from_millis(250)).await;
                            // Fetch the view again: the window may have
                            // closed during the wait, and a stale pointer
                            // must not be used.
                            cx.update(|window, _| native_view(window))
                                .map_err(|e| e.to_string())
                                .and_then(|v| v)
                                .and_then(|view| capture(view, &p))
                                .map(|()| eprintln!("halyard: saved {}", p.display()))
                        }
                        Err(e) => Err(e),
                    }
                }
                Step::Select(c1, r1, c2, r2) => this
                    .update(cx, |host, cx| host.script_select(c1, r1, c2, r2, cx))
                    .map_err(|e| e.to_string()),
                Step::Resize(w, h) => cx
                    .update(|window, _| window.resize(size(px(w), px(h))))
                    .map_err(|e| e.to_string()),
                Step::Dump => this
                    .update_in(cx, |host, window, cx| {
                        eprintln!("halyard: {}", host.script_dump(window, cx));
                    })
                    .map_err(|e| e.to_string()),
                Step::CloseButton => cx
                    .update(|window, _| native_view(window))
                    .map_err(|e| e.to_string())
                    .and_then(|v| v)
                    .and_then(press_close),
                Step::Activate => cx
                    .update(|window, cx| {
                        cx.activate(true);
                        window.activate_window();
                    })
                    .map_err(|e| e.to_string()),
                Step::Quit => cx.update(|_, cx| cx.quit()).map_err(|e| e.to_string()),
            };
            if let Err(e) = result {
                eprintln!("halyard: debug script step failed: {e}");
            }
        }
    })
    .detach();
}

/// Mouse event kinds a script can post.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseKind {
    /// Button pressed.
    Down,
    /// Moved with the button held.
    Dragged,
    /// Button released.
    Up,
}

/// Post a mouse event to the window owning `view`, at `m.x`, `m.y` in
/// window coordinates with the origin at the bottom left (AppKit's), the
/// way the window server delivers a real click. It goes through
/// `-[NSWindow sendEvent:]`, so GPUI sees it exactly like a mouse event.
#[cfg(target_os = "macos")]
// objc's msg_send! checks a `cargo-clippy` cfg that this crate does not declare.
#[allow(unexpected_cfgs)]
pub fn post_mouse(view: usize, m: Mouse) -> Result<(), String> {
    use objc::runtime::{Class, Object};
    use objc::{msg_send, sel, sel_impl};

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct NsPoint {
        x: f64,
        y: f64,
    }
    // NSEventType: left down 1, up 2, dragged 6.
    let event_type: u64 = match m.kind {
        MouseKind::Down => 1,
        MouseKind::Up => 2,
        MouseKind::Dragged => 6,
    };
    // NSEventModifierFlagShift, NSEventModifierFlagControl. `mouseEventWithType`
    // leaves the button number 0 (left) even for the right-button types, and
    // GPUI reads the button number; a Control-click is what GPUI maps to the
    // right button on macOS, so a scripted right click is one.
    let mut flags: u64 = if m.shift { 1 << 17 } else { 0 };
    if m.button == Button::Right {
        flags |= 1 << 18;
    }
    let view = view as *mut Object;
    let nsevent = Class::get("NSEvent").ok_or("no NSEvent class")?;
    let process_info = Class::get("NSProcessInfo").ok_or("no NSProcessInfo class")?;
    // SAFETY: `view` is the live NSView of an open window (fetched from the
    // window just before this call, on this thread). The messages are the
    // documented AppKit constructors and getters; `event` is autoreleased
    // and used before the enclosing run loop pass ends.
    unsafe {
        let ns_window: *mut Object = msg_send![view, window];
        if ns_window.is_null() {
            return Err("the view has no window".into());
        }
        let number: isize = msg_send![ns_window, windowNumber];
        let info: *mut Object = msg_send![process_info, processInfo];
        let now: f64 = msg_send![info, systemUptime];
        let nil: *mut Object = std::ptr::null_mut();
        let event: *mut Object = msg_send![nsevent,
            mouseEventWithType: event_type
            location: NsPoint { x: f64::from(m.x), y: f64::from(m.y) }
            modifierFlags: flags
            timestamp: now
            windowNumber: number
            context: nil
            eventNumber: 0isize
            clickCount: isize::from(m.clicks)
            pressure: 1.0f32];
        if event.is_null() {
            return Err("AppKit returned no event".into());
        }
        let () = msg_send![ns_window, sendEvent: event];
    }
    Ok(())
}

/// Press the close button of the window owning `view`, outside any GPUI
/// update (the window's should-close handler runs synchronously).
#[cfg(target_os = "macos")]
// objc's msg_send! checks a `cargo-clippy` cfg that this crate does not declare.
#[allow(unexpected_cfgs)]
pub fn press_close(view: usize) -> Result<(), String> {
    use objc::runtime::Object;
    use objc::{msg_send, sel, sel_impl};
    let view = view as *mut Object;
    // SAFETY: `view` is the live NSView of an open window, fetched just
    // before this call on this thread; `performClose:` takes a sender.
    unsafe {
        let ns_window: *mut Object = msg_send![view, window];
        if ns_window.is_null() {
            return Err("the view has no window".into());
        }
        let nil: *mut Object = std::ptr::null_mut();
        let () = msg_send![ns_window, performClose: nil];
    }
    Ok(())
}

/// Pressing the close button is only implemented on macOS.
#[cfg(not(target_os = "macos"))]
pub fn press_close(_view: usize) -> Result<(), String> {
    Err("pressing the close button is only implemented on macOS".into())
}

/// Posting mouse events is only implemented on macOS.
#[cfg(not(target_os = "macos"))]
pub fn post_mouse(_view: usize, _m: Mouse) -> Result<(), String> {
    Err("posting mouse events is only implemented on macOS".into())
}

/// The window's `NSView`, as an address that can leave the update closure.
#[cfg(target_os = "macos")]
pub fn native_view(window: &gpui::Window) -> Result<usize, String> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = HasWindowHandle::window_handle(window).map_err(|e| e.to_string())?;
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return Err("not an AppKit window".into());
    };
    Ok(appkit.ns_view.as_ptr() as usize)
}

/// Draw the pending frame now. GPUI stops its display link while the
/// window is occluded (for example behind the lock screen), so a capture
/// would otherwise show a stale frame. Asking the layer to display calls
/// GPUI's `displayLayer:` delegate, which renders synchronously. Must be
/// called outside any GPUI update, since that delegate updates the window.
#[cfg(target_os = "macos")]
// objc's msg_send! checks a `cargo-clippy` cfg that this crate does not declare.
#[allow(unexpected_cfgs)]
pub fn force_display(view: usize) {
    use objc::runtime::Object;
    use objc::{msg_send, sel, sel_impl};
    let view = view as *mut Object;
    // SAFETY: `view` is the live NSView of an open window (the caller just
    // read it from that window on this thread); `layer` is a getter and
    // `display` draws the layer.
    unsafe {
        let layer: *mut Object = msg_send![view, layer];
        if !layer.is_null() {
            let () = msg_send![layer, display];
        }
    }
}

/// Save a PNG of the window that owns `view`.
#[cfg(target_os = "macos")]
// objc's msg_send! checks a `cargo-clippy` cfg that this crate does not declare.
#[allow(unexpected_cfgs)]
pub fn capture(view: usize, path: &Path) -> Result<(), String> {
    use core_graphics::geometry::{CGPoint, CGRect, CGSize};
    use core_graphics::window::{
        create_image, kCGWindowImageBestResolution, kCGWindowImageBoundsIgnoreFraming,
        kCGWindowListOptionIncludingWindow,
    };
    use objc::runtime::Object;
    use objc::{msg_send, sel, sel_impl};

    let view = view as *mut Object;
    // SAFETY: as in `force_display`; `window` and `windowNumber` are getters.
    let number: isize = unsafe {
        let ns_window: *mut Object = msg_send![view, window];
        msg_send![ns_window, windowNumber]
    };
    let id = u32::try_from(number).map_err(|_| "bad window number".to_owned())?;
    let null_rect =
        CGRect::new(&CGPoint::new(f64::INFINITY, f64::INFINITY), &CGSize::new(0.0, 0.0));
    let image = create_image(
        null_rect,
        kCGWindowListOptionIncludingWindow,
        id,
        kCGWindowImageBoundsIgnoreFraming | kCGWindowImageBestResolution,
    )
    .ok_or("the window server returned no image")?;
    if image.bits_per_pixel() != 32 {
        return Err(format!("unexpected {} bits per pixel", image.bits_per_pixel()));
    }
    let (w, h, stride) = (image.width(), image.height(), image.bytes_per_row());
    let data = image.data();
    let bytes = data.bytes();
    // Window images are 32-bit little-endian BGRA; PNG wants RGBA.
    let mut rgba = Vec::with_capacity(w * h * 4);
    for row in 0..h {
        for px in bytes[row * stride..row * stride + w * 4].as_chunks::<4>().0 {
            rgba.extend_from_slice(&[px[2], px[1], px[0], 255]);
        }
    }
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut encoder = png::Encoder::new(
        std::io::BufWriter::new(file),
        u32::try_from(w).map_err(|e| e.to_string())?,
        u32::try_from(h).map_err(|e| e.to_string())?,
    );
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(&rgba).map_err(|e| e.to_string())
}

/// Screenshots are only implemented on macOS.
#[cfg(not(target_os = "macos"))]
pub fn native_view(_window: &gpui::Window) -> Result<usize, String> {
    Err("screenshots are only implemented on macOS".into())
}

/// Screenshots are only implemented on macOS.
#[cfg(not(target_os = "macos"))]
pub fn force_display(_view: usize) {}

/// Screenshots are only implemented on macOS.
#[cfg(not(target_os = "macos"))]
pub fn capture(_view: usize, _path: &Path) -> Result<(), String> {
    Err("screenshots are only implemented on macOS".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_command() {
        let s = "# comment\n\nsleep 250\ntype ls -la\nkey ctrl-c\nshot /tmp/a.png\nselect 0 1 4 1\nresize 640 400\ndump\naction split_right\nmousedown 1 2\nmousemove 3 4\nmouseup 5 6\nrightdown 7 8 shift\nrightup 7 8\nmousedown 9 10 clicks=2\ncopy /a /b\nquit\n";
        let mouse = |button, kind, x, y| Mouse { button, kind, x, y, clicks: 1, shift: false };
        assert_eq!(
            parse(s).unwrap(),
            vec![
                Step::Sleep(Duration::from_millis(250)),
                Step::Type("ls -la".into()),
                Step::Key("ctrl-c".into()),
                Step::Shot(PathBuf::from("/tmp/a.png")),
                Step::Select(0, 1, 4, 1),
                Step::Resize(640.0, 400.0),
                Step::Dump,
                Step::Action("split_right".into()),
                Step::Mouse(mouse(Button::Left, MouseKind::Down, 1.0, 2.0)),
                Step::Mouse(mouse(Button::Left, MouseKind::Dragged, 3.0, 4.0)),
                Step::Mouse(mouse(Button::Left, MouseKind::Up, 5.0, 6.0)),
                Step::Mouse(Mouse {
                    shift: true,
                    ..mouse(Button::Right, MouseKind::Down, 7.0, 8.0)
                }),
                Step::Mouse(mouse(Button::Right, MouseKind::Up, 7.0, 8.0)),
                Step::Mouse(Mouse { clicks: 2, ..mouse(Button::Left, MouseKind::Down, 9.0, 10.0) }),
                Step::Copy(PathBuf::from("/a"), PathBuf::from("/b")),
                Step::Quit,
            ]
        );
    }

    #[test]
    fn rejects_unknown_commands() {
        assert!(parse("jump 3").unwrap_err().contains("line 1"));
        assert!(parse("sleep soon").is_err());
        assert!(parse("select 1 2 3").is_err());
        assert!(parse("mousedown 1").is_err());
        assert!(parse("rightdown 1 2 ctrl").is_err(), "unknown modifier");
        assert!(parse("mousedown 1 2 clicks=x").is_err());
        assert!(parse("copy /only-one").is_err());
    }
}
