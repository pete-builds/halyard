//! Debug-only scripted driving, for verifying the window without a person
//! at the keyboard. Compiled only with the `debug-script` feature.
//!
//! `HALYARD_DEBUG_SCRIPT=<file>` runs one command per line after launch:
//!
//! - `sleep <ms>`: wait.
//! - `type <text>`: type each character through the normal text path.
//! - `key <keystroke>`: press a key through the normal key routing, written
//!   in GPUI keystroke syntax (`enter`, `ctrl-c`, `escape`, `space`).
//! - `shot <path.png>`: save a PNG of this window (macOS only; captures the
//!   app's own window, so no screen recording permission is needed).
//! - `select <col> <row> <col> <row>`: select from one cell to another
//!   (through the session, not the mouse path).
//! - `resize <width> <height>`: resize the window's content (logical px).
//! - `dump`: print the latest frame's text and the render count to stderr.
//! - `quit`: quit the app.
//!
//! Lines starting with `#` and blank lines are ignored.

use std::path::{Path, PathBuf};
use std::time::Duration;

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
    /// Quit.
    Quit,
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
                let size: Vec<f32> = arg
                    .split_whitespace()
                    .map(str::parse)
                    .collect::<Result<_, _>>()
                    .map_err(|e| format!("line {}: {e}", n + 1))?;
                let [w, h] = size[..] else {
                    return Err(format!("line {}: resize needs two numbers", n + 1));
                };
                Step::Resize(w, h)
            }
            "dump" => Step::Dump,
            "quit" => Step::Quit,
            other => return Err(format!("line {}: unknown command `{other}`", n + 1)),
        });
    }
    Ok(steps)
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
        let s = "# comment\n\nsleep 250\ntype ls -la\nkey ctrl-c\nshot /tmp/a.png\nselect 0 1 4 1\nresize 640 400\ndump\nquit\n";
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
                Step::Quit,
            ]
        );
    }

    #[test]
    fn rejects_unknown_commands() {
        assert!(parse("jump 3").unwrap_err().contains("line 1"));
        assert!(parse("sleep soon").is_err());
        assert!(parse("select 1 2 3").is_err());
    }
}
